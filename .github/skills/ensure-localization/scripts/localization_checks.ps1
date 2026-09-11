<#
.SYNOPSIS
    Read-only file-based localization checks for `.resw` and flat localization `.yml` files.

.DESCRIPTION
    Public checks:
      Test-ResourceSyntax(File) - validate one supported localization file.
      Test-RequiredKeys(SourceFile, TargetFile, Keys optional) - find missing or stale keys.
      Test-PlaceholderParity(SourceFile, TargetFile, Keys optional) - preserve placeholder identity and count.
      Test-LockedContent(SourceFile, TargetFile, Locale when scoped, Keys optional) - preserve source-defined locked values or tokens.
      Test-ResourceEncoding(File, OriginalFile optional) - require UTF-8 and enforce the expected BOM behavior.
      Test-PseudoLocale(SourceFile, TargetFile, Locale, Keys optional) - catch plain-English fallback or wrapper mistakes.

    The checks only read caller-supplied files and write exactly one JSON bundle
    to stdout per CLI invocation. CLI key scoping is supplied with -KeysJson;
    public PowerShell functions continue to accept [string[]] -Keys directly.
    Dot-source this script to batch many checks in one PowerShell process:
    each completed check returns one bundle object with check, status, exitCode,
    summary, and results. Invalid required arguments can throw; let the batch fail.
    They do not perform Git, PR, SHA, auth, discovery,
    translation-quality, or code-edit-policy work.

    Exit codes:
      0  PASS
      20 FIXABLE
      30 BLOCKED
      64 INVALID_INPUT
#>
[CmdletBinding(PositionalBinding = $false)]
param(
    [string]$Check,
    [string]$File,
    [string]$SourceFile,
    [string]$TargetFile,
    [string]$OriginalFile,
    [string]$Locale,
    [string]$KeysJson,
    [Parameter(ValueFromRemainingArguments)]
    [AllowEmptyCollection()]
    [string[]]$UnexpectedArguments
)

$script:TopLevelBoundParameters = @{} + $PSBoundParameters

$script:SupportedChecks = @(
    'Test-ResourceSyntax',
    'Test-RequiredKeys',
    'Test-PlaceholderParity',
    'Test-LockedContent',
    'Test-ResourceEncoding',
    'Test-PseudoLocale'
)
$script:SupportedPseudoLocales = @('qps-ploc', 'qps-ploca', 'qps-plocm')
$script:ExitCodes = @{
    Pass = 0
    Fixable = 20
    Blocked = 30
    InvalidInput = 64
}

function New-CheckRecord {
    param(
        [Parameter(Mandatory)][ValidateSet('PASS', 'FIXABLE', 'BLOCKED')][string]$Status,
        [Parameter(Mandatory)][string]$CheckName,
        [string]$File,
        [string]$Resource,
        $Expected,
        $Observed,
        [Parameter(Mandatory)][string]$Message,
        [string]$SuggestedAction
    )

    return [pscustomobject][ordered]@{
        check = $CheckName
        status = $Status
        file = $File
        resource = $Resource
        expected = $Expected
        observed = $Observed
        message = $Message
        suggestedAction = $SuggestedAction
    }
}

function Complete-CheckBundle {
    param(
        [Parameter(Mandatory)][string]$CheckName,
        [AllowEmptyCollection()][object[]]$Results,
        [Parameter(Mandatory)][string]$PassMessage,
        [string]$PassFile,
        [string]$PassResource
    )

    $resolved = @($Results | Where-Object { $null -ne $_ })
    if ($resolved.Count -eq 0) {
        $resolved = @(
            (New-CheckRecord -Status 'PASS' -CheckName $CheckName -File $PassFile -Resource $PassResource `
                -Expected $null -Observed $null -Message $PassMessage -SuggestedAction $null)
        )
    }

    $status = if (@($resolved | Where-Object { $_.status -eq 'BLOCKED' }).Count -gt 0) {
        'BLOCKED'
    } elseif (@($resolved | Where-Object { $_.status -eq 'FIXABLE' }).Count -gt 0) {
        'FIXABLE'
    } else {
        'PASS'
    }

    $exitCode = switch ($status) {
        'BLOCKED' { $script:ExitCodes.Blocked; break }
        'FIXABLE' { $script:ExitCodes.Fixable; break }
        default { $script:ExitCodes.Pass }
    }

    return [pscustomobject][ordered]@{
        check = $CheckName
        status = $status
        exitCode = $exitCode
        summary = [ordered]@{
            totalCount = $resolved.Count
            passCount = @($resolved | Where-Object { $_.status -eq 'PASS' }).Count
            fixableCount = @($resolved | Where-Object { $_.status -eq 'FIXABLE' }).Count
            blockedCount = @($resolved | Where-Object { $_.status -eq 'BLOCKED' }).Count
        }
        results = @($resolved)
    }
}

function New-InvalidInputBundle {
    param(
        [string]$CheckName,
        [Parameter(Mandatory)][string]$Message
    )

    return [pscustomobject][ordered]@{
        check = $CheckName
        status = 'INVALID_INPUT'
        exitCode = $script:ExitCodes.InvalidInput
        summary = [ordered]@{
            totalCount = 0
            passCount = 0
            fixableCount = 0
            blockedCount = 0
        }
        message = $Message
        results = @()
    }
}

function New-BlockedBundle {
    param(
        [Parameter(Mandatory)][string]$CheckName,
        [string]$File,
        [string]$Resource,
        [Parameter(Mandatory)][string]$Message,
        [Parameter(Mandatory)][string]$SuggestedAction,
        $Expected = $null,
        $Observed = $null
    )

    return Complete-CheckBundle -CheckName $CheckName -Results @(
        New-CheckRecord -Status 'BLOCKED' -CheckName $CheckName -File $File -Resource $Resource `
            -Expected $Expected -Observed $Observed -Message $Message -SuggestedAction $SuggestedAction
    ) -PassMessage 'blocked'
}

function Assert-RequiredPath {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$ParameterName
    )

    if ([string]::IsNullOrWhiteSpace($Path)) {
        throw [System.ArgumentException]::new("$ParameterName is required.")
    }

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw [System.ArgumentException]::new("$ParameterName '$Path' was not found.")
    }

    return [System.IO.Path]::GetFullPath($Path)
}

function Assert-RequiredValue {
    param(
        [string]$Value,
        [string]$ParameterName
    )

    if ([string]::IsNullOrWhiteSpace($Value)) {
        throw [System.ArgumentException]::new("$ParameterName is required.")
    }

    return $Value
}

function Get-FileBytes {
    param([Parameter(Mandatory)][string]$Path)

    return [System.IO.File]::ReadAllBytes($Path)
}

function Test-HasUtf8Bom {
    param([byte[]]$Bytes)

    return $null -ne $Bytes -and $Bytes.Length -ge 3 -and $Bytes[0] -eq 0xEF -and $Bytes[1] -eq 0xBB -and $Bytes[2] -eq 0xBF
}

function Get-Utf8Text {
    param(
        [Parameter(Mandatory)][byte[]]$Bytes,
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$Kind
    )

    if ($Bytes -contains 0) {
        throw [System.InvalidOperationException]::new("$Path is not valid UTF-8 text for $Kind.")
    }

    $encoding = [System.Text.UTF8Encoding]::new($false, $true)
    $offset = if (Test-HasUtf8Bom -Bytes $Bytes) { 3 } else { 0 }
    try {
        return $encoding.GetString($Bytes, $offset, $Bytes.Length - $offset)
    } catch [System.Text.DecoderFallbackException] {
        throw [System.InvalidOperationException]::new("$Path is not valid UTF-8 text for $Kind.")
    }
}

function Get-LocalizationFileKind {
    param([Parameter(Mandatory)][string]$Path)

    $extension = [System.IO.Path]::GetExtension($Path)
    switch -Regex ($extension) {
        '^\.resw$' { return 'resw' }
        '^\.yml$' { return 'wta' }
        default { return $null }
    }
}

function Normalize-CommentText {
    param([string]$Text)

    return ([regex]::Replace($Text.Trim(), '\s+', ' '))
}

function Test-WtaSectionHeaderComment {
    param([string]$Line)

    return [regex]::IsMatch($Line, '^\s*#\s*──\s+.+?\s+─{2,}\s*$')
}

function Test-ContainsLockDirective {
    param([string]$Comment)

    return -not [string]::IsNullOrWhiteSpace($Comment) -and
        [regex]::IsMatch($Comment, '\{Locked(?:(?:"[^"]*")|[^}])*\}')
}

function Parse-QuotedTokenList {
    param(
        [string]$Text,
        [switch]$AllowTrailingText
    )

    $tokens = [System.Collections.Generic.List[string]]::new()
    $index = 0
    while ($index -lt $Text.Length) {
        while ($index -lt $Text.Length -and [char]::IsWhiteSpace($Text[$index])) {
            $index++
        }
        if ($index -ge $Text.Length) {
            break
        }
        if ($Text[$index] -ne '"') {
            if ($AllowTrailingText) {
                break
            }
            throw [System.InvalidOperationException]::new('Locked token lists must contain comma-delimited quoted literals.')
        }

        $tokenStart = ++$index
        $tokenEnd = -1
        $nextIndex = -1
        while ($index -lt $Text.Length) {
            if ($Text[$index] -eq '"') {
                $afterQuote = $index + 1
                while ($afterQuote -lt $Text.Length -and [char]::IsWhiteSpace($Text[$afterQuote])) {
                    $afterQuote++
                }
                if ($afterQuote -ge $Text.Length -or $Text[$afterQuote] -eq ',' -or
                    ($AllowTrailingText -and $afterQuote -gt $index + 1)) {
                    $tokenEnd = $index
                    $nextIndex = $afterQuote
                    break
                }
            }
            $index++
        }

        if ($tokenEnd -lt 0) {
            throw [System.InvalidOperationException]::new('Locked token lists contain an unterminated quoted literal.')
        }

        $tokens.Add($Text.Substring($tokenStart, $tokenEnd - $tokenStart))
        $index = $nextIndex
        if ($index -ge $Text.Length) {
            break
        }
        if ($Text[$index] -eq ',') {
            $index++
            continue
        }
        if ($AllowTrailingText) {
            break
        }
        throw [System.InvalidOperationException]::new('Locked token lists must separate quoted literals with commas.')
    }
    return @($tokens.ToArray())
}

function Parse-LeadingQuotedTokenList {
    param([string]$Text)

    if ($Text -notmatch '^\s*"') {
        return @()
    }

    return @(Parse-QuotedTokenList -Text $Text -AllowTrailingText)
}

function Test-IsLocaleScopeName {
    param([string]$Value)

    if ([string]::IsNullOrWhiteSpace($Value)) {
        return $false
    }

    $candidate = $Value.Trim()
    if ($script:SupportedPseudoLocales -contains $candidate) {
        return $true
    }

    return [regex]::IsMatch($candidate, '^(?:[a-z]{2,3}|[A-Za-z]{2,8}(?:-[A-Za-z0-9]{2,8})+)$')
}

function Test-IsLegacyBareLockToken {
    param([string]$Value)

    if ([string]::IsNullOrWhiteSpace($Value)) {
        return $false
    }

    return [regex]::IsMatch($Value.Trim(), '^[A-Z][A-Z0-9_]*$')
}

function Resolve-UnquotedLockPayload {
    param([string]$Payload)

    $parts = @($Payload -split '\s*,\s*' | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
    if ($parts.Count -eq 0) {
        return [pscustomobject]@{
            Kind = 'invalid'
            Values = @()
            Reason = 'Unquoted lock directives must be locale scopes or use the canonical quoted token form {Locked="token"}.'
        }
    }

    $trimmedParts = @($parts | ForEach-Object { $_.Trim() })
    if (@($trimmedParts | Where-Object { -not (Test-IsLocaleScopeName -Value $_) }).Count -eq 0) {
        return [pscustomobject]@{
            Kind = 'locale'
            Values = $trimmedParts
            Reason = $null
        }
    }

    if ($trimmedParts.Count -eq 1 -and (Test-IsLegacyBareLockToken -Value $trimmedParts[0])) {
        return [pscustomobject]@{
            Kind = 'token'
            Values = $trimmedParts
            Reason = $null
        }
    }

    return [pscustomobject]@{
        Kind = 'invalid'
        Values = @()
        Reason = "Unquoted lock directive payload '$Payload' is ambiguous. Use locale scopes like {Locked=qps-ploc} or quote literal tokens like {Locked=""token""}."
    }
}

function Split-WtaScalarAndComment {
    param(
        [Parameter(Mandatory)][string]$Value,
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][int]$LineNumber
    )

    if ([string]::IsNullOrWhiteSpace($Value)) {
        throw [System.InvalidOperationException]::new("${Path}:$LineNumber uses an unsupported YAML structure.")
    }

    if ($Value.StartsWith('"')) {
        $escaped = $false
        for ($index = 1; $index -lt $Value.Length; $index++) {
            $character = $Value[$index]
            if ($escaped) {
                $escaped = $false
                continue
            }
            if ($character -eq '\') {
                $escaped = $true
                continue
            }
            if ($character -eq '"') {
                $scalar = $Value.Substring(0, $index + 1)
                $remainder = $Value.Substring($index + 1).TrimStart()
                if ($remainder -and -not $remainder.StartsWith('#')) {
                    throw [System.InvalidOperationException]::new("${Path}:$LineNumber uses an unsupported YAML structure.")
                }
                return [pscustomobject]@{
                    Scalar = $scalar
                    InlineComment = if ($remainder) { Normalize-CommentText -Text $remainder } else { $null }
                }
            }
        }
        throw [System.InvalidOperationException]::new("${Path}:$LineNumber contains an unterminated YAML string.")
    }

    if ($Value.StartsWith("'")) {
        $index = 1
        while ($index -lt $Value.Length) {
            if ($Value[$index] -eq "'") {
                if ($index + 1 -lt $Value.Length -and $Value[$index + 1] -eq "'") {
                    $index += 2
                    continue
                }
                $scalar = $Value.Substring(0, $index + 1)
                $remainder = $Value.Substring($index + 1).TrimStart()
                if ($remainder -and -not $remainder.StartsWith('#')) {
                    throw [System.InvalidOperationException]::new("${Path}:$LineNumber uses an unsupported YAML structure.")
                }
                return [pscustomobject]@{
                    Scalar = $scalar
                    InlineComment = if ($remainder) { Normalize-CommentText -Text $remainder } else { $null }
                }
            }
            $index++
        }
        throw [System.InvalidOperationException]::new("${Path}:$LineNumber contains an unterminated YAML string.")
    }

    $commentIndex = $Value.IndexOf(' #')
    if ($commentIndex -ge 0) {
        return [pscustomobject]@{
            Scalar = $Value.Substring(0, $commentIndex).TrimEnd()
            InlineComment = Normalize-CommentText -Text $Value.Substring($commentIndex + 1)
        }
    }

    return [pscustomobject]@{
        Scalar = $Value.TrimEnd()
        InlineComment = $null
    }
}

function ConvertFrom-YamlScalar {
    param(
        [Parameter(Mandatory)][string]$Scalar,
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][int]$LineNumber
    )

    if ($Scalar.StartsWith('"')) {
        try {
            return (ConvertFrom-Json -InputObject $Scalar -ErrorAction Stop)
        } catch {
            throw [System.InvalidOperationException]::new("${Path}:$LineNumber contains an unsupported YAML string literal.")
        }
    }

    if ($Scalar.StartsWith("'") -and $Scalar.EndsWith("'")) {
        return $Scalar.Substring(1, $Scalar.Length - 2).Replace("''", "'")
    }

    if ([string]::IsNullOrWhiteSpace($Scalar)) {
        throw [System.InvalidOperationException]::new("${Path}:$LineNumber uses an unsupported YAML structure.")
    }

    $implicitScalar = '(?i)^(?:null|~|true|false|yes|no|on|off|[-+]?\.(?:inf|nan)|[-+]?0(?:x[0-9a-f_]+|o[0-7_]+|b[01_]+)|[-+]?(?:[0-9][0-9_]*(?:\.[0-9_]*)?|\.[0-9_]+)(?:e[-+]?[0-9]+)?)$'
    if ($Scalar -match '^[\[\]{},&*!|>"%@`#]' -or
        $Scalar -match '^[-?:](?:\s|$)' -or
        $Scalar -match ':\s' -or
        $Scalar -match $implicitScalar) {
        throw [System.InvalidOperationException]::new(
            "${Path}:$LineNumber uses an unsupported or implicitly typed YAML plain scalar. Quote string values that contain YAML indicators or resemble null, Boolean, or numeric values."
        )
    }

    return $Scalar
}

function Parse-WtaEntryLine {
    param(
        [Parameter(Mandatory)][string]$Line,
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][int]$LineNumber
    )

    $match = [regex]::Match($Line, '^([A-Za-z0-9_.-]+)\s*:\s*(.*)$')
    if (-not $match.Success) {
        throw [System.InvalidOperationException]::new("${Path}:$LineNumber uses an unsupported YAML structure.")
    }

    $split = Split-WtaScalarAndComment -Value $match.Groups[2].Value -Path $Path -LineNumber $LineNumber
    return [pscustomobject]@{
        Key = $match.Groups[1].Value
        Value = ConvertFrom-YamlScalar -Scalar $split.Scalar -Path $Path -LineNumber $LineNumber
        InlineComment = $split.InlineComment
    }
}

function Read-WtaLocaleEntries {
    param(
        [Parameter(Mandatory)][byte[]]$Bytes,
        [Parameter(Mandatory)][string]$Path
    )

    $text = Get-Utf8Text -Bytes $Bytes -Path $Path -Kind 'WTA locale file'
    $lines = @($text -split "`r`n|`n|`r", 0)
    $entries = [System.Collections.Generic.Dictionary[string, object]]::new([System.StringComparer]::Ordinal)
    $fileLockComments = @()
    $pendingLockComments = [System.Collections.Generic.List[string]]::new()
    $sectionLockComments = @()
    $blockLockComments = @()
    $pendingHasSectionHeader = $false
    $seenEntry = $false
    $seenEntrySinceBlank = $false

    for ($index = 0; $index -lt $lines.Count; $index++) {
        $lineNumber = $index + 1
        $line = $lines[$index]
        $trimmed = $line.Trim()

        if ([string]::IsNullOrWhiteSpace($trimmed)) {
            if (-not $seenEntry -and $pendingLockComments.Count -gt 0 -and $fileLockComments.Count -eq 0) {
                $fileLockComments = @($pendingLockComments.ToArray())
            }
            $pendingLockComments.Clear()
            $sectionLockComments = @()
            $blockLockComments = @()
            $pendingHasSectionHeader = $false
            $seenEntrySinceBlank = $false
            continue
        }

        if ($trimmed.StartsWith('#')) {
            if (Test-WtaSectionHeaderComment -Line $trimmed) {
                $pendingHasSectionHeader = $true
            }

            $commentText = Normalize-CommentText -Text $trimmed
            if (Test-ContainsLockDirective -Comment $commentText) {
                $pendingLockComments.Add($commentText)
            }
            continue
        }

        $seenEntry = $true
        $entryLockComments = @()

        if ($pendingHasSectionHeader) {
            $sectionLockComments = @($pendingLockComments.ToArray())
            $blockLockComments = @()
        } elseif ($pendingLockComments.Count -gt 0) {
            if ($seenEntrySinceBlank) {
                $entryLockComments = @($pendingLockComments.ToArray())
            } else {
                $blockLockComments = @($pendingLockComments.ToArray())
            }
        }

        $entry = Parse-WtaEntryLine -Line $line -Path $Path -LineNumber $lineNumber
        if ($entries.ContainsKey($entry.Key)) {
            throw [System.InvalidOperationException]::new("$Path contains duplicate key '$($entry.Key)'.")
        }

        $comments = [System.Collections.Generic.List[string]]::new()
        foreach ($comment in @($sectionLockComments + $blockLockComments + $entryLockComments)) {
            if (-not [string]::IsNullOrWhiteSpace($comment)) {
                $comments.Add($comment)
            }
        }
        if (Test-ContainsLockDirective -Comment $entry.InlineComment) {
            $comments.Add($entry.InlineComment)
        }

        $entries[$entry.Key] = [pscustomobject]@{
            Key = $entry.Key
            Value = [string]$entry.Value
            Comments = @($comments.ToArray())
            InheritedTokenComments = @($fileLockComments)
        }
        $pendingLockComments.Clear()
        $blockLockComments = @()
        $pendingHasSectionHeader = $false
        $seenEntrySinceBlank = $true
    }

    return [pscustomobject]@{
        Kind = 'wta'
        Path = $Path
        HasBom = Test-HasUtf8Bom -Bytes $Bytes
        Entries = $entries
    }
}

function Read-ReswResources {
    param(
        [Parameter(Mandatory)][byte[]]$Bytes,
        [Parameter(Mandatory)][string]$Path
    )

    $null = Get-Utf8Text -Bytes $Bytes -Path $Path -Kind '.resw'

    $settings = [System.Xml.XmlReaderSettings]::new()
    $settings.DtdProcessing = [System.Xml.DtdProcessing]::Prohibit
    $settings.XmlResolver = $null

    $stream = [System.IO.MemoryStream]::new($Bytes, $false)
    try {
        $reader = [System.Xml.XmlReader]::Create($stream, $settings)
        try {
            $document = [System.Xml.XmlDocument]::new()
            $document.PreserveWhitespace = $true
            $document.Load($reader)
        } finally {
            $reader.Dispose()
        }
    } catch {
        throw [System.InvalidOperationException]::new("$Path is not well-formed XML. $($_.Exception.Message)")
    } finally {
        $stream.Dispose()
    }

    $root = $document.DocumentElement
    if ($null -eq $root -or $root.LocalName -cne 'root') {
        throw [System.InvalidOperationException]::new("$Path is not a supported .resw file. Expected a <root> document element.")
    }

    $allowedRootChildren = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
    foreach ($name in @('data', 'resheader', 'metadata', 'assembly', 'schema')) {
        $null = $allowedRootChildren.Add($name)
    }

    $entries = [System.Collections.Generic.Dictionary[string, object]]::new([System.StringComparer]::Ordinal)
    foreach ($childNode in @($root.ChildNodes)) {
        if ($childNode.NodeType -ne [System.Xml.XmlNodeType]::Element) {
            continue
        }

        $node = [System.Xml.XmlElement]$childNode
        if (-not $allowedRootChildren.Contains($node.LocalName)) {
            throw [System.InvalidOperationException]::new("$Path contains unsupported <$($node.LocalName)> content under <root>.")
        }

        if ($node.LocalName -cne 'data') {
            continue
        }

        $name = [string]$node.GetAttribute('name')
        if ([string]::IsNullOrWhiteSpace($name)) {
            throw [System.InvalidOperationException]::new("$Path contains a <data> entry without a non-empty name attribute.")
        }
        if ($entries.ContainsKey($name)) {
            throw [System.InvalidOperationException]::new("$Path contains duplicate resource '$name'.")
        }

        $elementChildren = @($node.ChildNodes | Where-Object { $_.NodeType -eq [System.Xml.XmlNodeType]::Element })
        foreach ($elementChild in $elementChildren) {
            if ($elementChild.LocalName -cnotin @('value', 'comment')) {
                throw [System.InvalidOperationException]::new("$Path resource '$name' uses an unsupported <$($elementChild.LocalName)> child element.")
            }
        }

        $valueNodes = @($node.SelectNodes('./value'))
        if ($valueNodes.Count -ne 1) {
            throw [System.InvalidOperationException]::new("$Path resource '$name' must contain exactly one <value> element.")
        }

        $commentNodes = @($node.SelectNodes('./comment'))
        if ($commentNodes.Count -gt 1) {
            throw [System.InvalidOperationException]::new("$Path resource '$name' must not contain more than one <comment> element.")
        }

        $valueNode = $valueNodes[0]
        $commentNode = if ($commentNodes.Count -eq 1) { $commentNodes[0] } else { $null }
        $comments = if ($null -ne $commentNode -and -not [string]::IsNullOrWhiteSpace($commentNode.InnerText)) {
            @([string]$commentNode.InnerText)
        } else {
            @()
        }

        $entries[$name] = [pscustomobject]@{
            Key = $name
            Value = if ($null -ne $valueNode) { [string]$valueNode.InnerText } else { '' }
            Comments = $comments
            InheritedTokenComments = @()
        }
    }

    return [pscustomobject]@{
        Kind = 'resw'
        Path = $Path
        HasBom = Test-HasUtf8Bom -Bytes $Bytes
        Entries = $entries
    }
}

function Read-LocalizationFile {
    param([Parameter(Mandatory)][string]$Path)

    $resolvedPath = Assert-RequiredPath -Path $Path -ParameterName 'File'
    $kind = Get-LocalizationFileKind -Path $resolvedPath
    if ($null -eq $kind) {
        throw [System.InvalidOperationException]::new("$resolvedPath uses an unsupported localization format. Supported formats: .resw and WTA-style .yml.")
    }

    $bytes = Get-FileBytes -Path $resolvedPath
    switch ($kind) {
        'resw' { return Read-ReswResources -Bytes $bytes -Path $resolvedPath }
        'wta' { return Read-WtaLocaleEntries -Bytes $bytes -Path $resolvedPath }
        default {
            throw [System.InvalidOperationException]::new("$resolvedPath uses an unsupported localization format.")
        }
    }
}

function Read-LocalizationPair {
    param(
        [Parameter(Mandatory)][string]$SourceFile,
        [Parameter(Mandatory)][string]$TargetFile
    )

    $source = Read-LocalizationFile -Path (Assert-RequiredPath -Path $SourceFile -ParameterName 'SourceFile')
    $target = Read-LocalizationFile -Path (Assert-RequiredPath -Path $TargetFile -ParameterName 'TargetFile')

    if ($source.Kind -ne $target.Kind) {
        throw [System.InvalidOperationException]::new('SourceFile and TargetFile must use the same supported localization format.')
    }

    return [pscustomobject]@{
        Source = $source
        Target = $target
    }
}

function Get-ScopeKeys {
    param(
        [Parameter(Mandatory)]$SourceEntries,
        [Parameter(Mandatory)]$TargetEntries,
        [string[]]$Keys,
        [bool]$KeysWereSupplied = $false,
        [switch]$IntersectionWhenUnscoped
    )

    if ($KeysWereSupplied) {
        if ($null -eq $Keys -or $Keys.Count -eq 0) {
            throw [System.ArgumentException]::new('Keys must be a non-empty array of non-blank strings when supplied. Omit -Keys to scope the whole file.')
        }

        $set = [System.Collections.Generic.SortedSet[string]]::new([System.StringComparer]::Ordinal)
        foreach ($key in $Keys) {
            if ([string]::IsNullOrWhiteSpace($key)) {
                throw [System.ArgumentException]::new('Keys must be a non-empty array of non-blank strings when supplied. Omit -Keys to scope the whole file.')
            }

            $null = $set.Add($key)
        }

        return @($set)
    }

    $set = [System.Collections.Generic.SortedSet[string]]::new([System.StringComparer]::Ordinal)
    foreach ($key in $SourceEntries.Keys) {
        if (-not $IntersectionWhenUnscoped) {
            $null = $set.Add($key)
        } elseif ($TargetEntries.ContainsKey($key)) {
            $null = $set.Add($key)
        }
    }
    if (-not $IntersectionWhenUnscoped) {
        foreach ($key in $TargetEntries.Keys) {
            $null = $set.Add($key)
        }
    }
    return @($set)
}

function Resolve-LockPolicy {
    param(
        [string[]]$Comments,
        [string[]]$InheritedTokenComments,
        [string]$Locale
    )

    $tokens = [System.Collections.Generic.SortedSet[string]]::new([System.StringComparer]::Ordinal)
    $fullLock = $false
    $needsLocale = $false
    $blockedReason = $null

    foreach ($commentSet in @(
        [pscustomobject]@{ Items = @($Comments); AllowFullLock = $true },
        [pscustomobject]@{ Items = @($InheritedTokenComments); AllowFullLock = $false }
    )) {
        foreach ($comment in @($commentSet.Items | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })) {
            foreach ($match in [regex]::Matches($comment, '\{Locked(?<body>(?:(?:"[^"]*")|[^}])*)\}')) {
                $body = $match.Groups['body'].Value.Trim()
                if ([string]::IsNullOrWhiteSpace($body)) {
                    if ($commentSet.AllowFullLock) {
                        $fullLock = $true
                    } elseif (-not $blockedReason) {
                        $blockedReason = 'File-level lock directives must quote the specific token(s) they preserve.'
                    }
                    continue
                }

                if (-not $body.StartsWith('=')) {
                    continue
                }

                $payload = $body.Substring(1).Trim()
                if ($payload.StartsWith('"')) {
                    foreach ($token in Parse-QuotedTokenList -Text $payload) {
                        $null = $tokens.Add($token)
                    }
                    continue
                }

                $payloadResolution = Resolve-UnquotedLockPayload -Payload $payload
                if ($payloadResolution.Kind -eq 'token') {
                    foreach ($token in $payloadResolution.Values) {
                        $null = $tokens.Add($token)
                    }
                    continue
                }

                if ($payloadResolution.Kind -eq 'invalid') {
                    if (-not $blockedReason) {
                        $blockedReason = $payloadResolution.Reason
                    }
                    continue
                }

                $needsLocale = $true
                if ([string]::IsNullOrWhiteSpace($Locale)) {
                    continue
                }

                $scopedLocales = @($payloadResolution.Values)
                if ($scopedLocales -notcontains $Locale) {
                    continue
                }

                $suffix = $comment.Substring($match.Index + $match.Length)
                $scopedTokens = @(Parse-LeadingQuotedTokenList -Text $suffix)
                if ($scopedTokens.Count -eq 0) {
                    if ($commentSet.AllowFullLock) {
                        $fullLock = $true
                    } elseif (-not $blockedReason) {
                        $blockedReason = "File-level locale-scoped locks must quote the specific token(s) they preserve for '$Locale'."
                    }
                    continue
                }

                foreach ($token in $scopedTokens) {
                    $null = $tokens.Add($token)
                }
            }
        }
    }

    return [pscustomobject]@{
        FullLock = $fullLock
        Tokens = @($tokens)
        NeedsLocale = $needsLocale
        BlockedReason = $blockedReason
    }
}

function Get-PlaceholderTokens {
    param([string]$Value)

    if ($null -eq $Value) {
        return @()
    }

    # Keep this intentionally narrow to the placeholder formats the repository
    # actually ships today: %{name}, %s, {}, and .NET-style indexed braces.
    $sanitized = $Value.Replace('{{', '  ').Replace('}}', '  ')
    $tokens = [System.Collections.Generic.List[string]]::new()

    foreach ($match in [regex]::Matches($sanitized, '%\{[^}]+\}|\{(?:\d+(?:\s*,\s*-?\d+)?(?:\s*:[^}]*)?)?\}')) {
        $tokens.Add($match.Value)
    }

    foreach ($match in [regex]::Matches($Value, '(?<!%)%(?:%%)*s')) {
        $tokens.Add('%s')
    }

    return @($tokens.ToArray())
}

function Format-TokenMultiset {
    param([string[]]$Tokens)

    if ($null -eq $Tokens -or $Tokens.Count -eq 0) {
        return '<none>'
    }

    $counts = [System.Collections.Generic.SortedDictionary[string, int]]::new([System.StringComparer]::Ordinal)
    foreach ($token in $Tokens) {
        if ($counts.ContainsKey($token)) {
            $counts[$token] = $counts[$token] + 1
        } else {
            $counts[$token] = 1
        }
    }

    return @(
        $counts.GetEnumerator() | ForEach-Object {
            if ($_.Value -gt 1) { '{0} × {1}' -f $_.Key, $_.Value } else { $_.Key }
        }
    ) -join ', '
}

function Remove-InvariantSegments {
    param(
        [string]$Value,
        [string[]]$LockedTokens
    )

    $result = if ($null -eq $Value) { '' } else { [string]$Value }
    foreach ($token in @($LockedTokens + (Get-PlaceholderTokens -Value $Value)) | Sort-Object Length -Descending) {
        if (-not [string]::IsNullOrEmpty($token)) {
            $result = $result.Replace($token, ' ')
        }
    }

    return [regex]::Replace($result, '[\p{P}\p{S}\s]+', '')
}

function Remove-ExpectedPseudoWrapper {
    param(
        [Parameter(Mandatory)][string]$Locale,
        [string]$Value
    )

    $text = if ($null -eq $Value) { '' } else { [string]$Value }
    switch ($Locale) {
        'qps-ploc' {
            if ($text.Length -ge 2 -and $text.StartsWith('[') -and $text.EndsWith(']')) {
                return $text.Substring(1, $text.Length - 2)
            }
        }
        'qps-ploca' {
            if ($text.Length -ge 8 -and $text.StartsWith('[!!_') -and $text.EndsWith('_!!]')) {
                return $text.Substring(4, $text.Length - 8)
            }
        }
        'qps-plocm' {
            if ($text.Length -ge 8 -and $text.StartsWith('[!! ') -and $text.EndsWith(' !!]')) {
                return $text.Substring(4, $text.Length - 8)
            }
        }
    }

    return $text
}

function Get-PseudoLetterSignal {
    param(
        [string]$Value,
        [string[]]$LockedTokens,
        [Parameter(Mandatory)][string]$Locale
    )

    $text = if ($null -eq $Value) { '' } else { [string]$Value }
    foreach ($token in @($LockedTokens + (Get-PlaceholderTokens -Value $Value)) | Sort-Object Length -Descending) {
        if (-not [string]::IsNullOrEmpty($token)) {
            $text = $text.Replace($token, ' ')
        }
    }

    $text = Remove-ExpectedPseudoWrapper -Locale $Locale -Value $text
    $letters = [System.Text.StringBuilder]::new()
    foreach ($match in [regex]::Matches($text, '\p{L}+')) {
        [void]$letters.Append($match.Value)
    }

    return $letters.ToString().ToUpperInvariant()
}

function Test-ContainsDirectionalPseudoSignal {
    param(
        [string]$Value,
        [string[]]$LockedTokens,
        [Parameter(Mandatory)][string]$Locale
    )

    $text = if ($null -eq $Value) { '' } else { [string]$Value }
    foreach ($token in @($LockedTokens + (Get-PlaceholderTokens -Value $Value)) | Sort-Object Length -Descending) {
        if (-not [string]::IsNullOrEmpty($token)) {
            $text = $text.Replace($token, ' ')
        }
    }

    $text = Remove-ExpectedPseudoWrapper -Locale $Locale -Value $text
    foreach ($codePoint in @(0x061C, 0x200E, 0x200F, 0x202A, 0x202B, 0x202C, 0x202D, 0x202E, 0x2066, 0x2067, 0x2068, 0x2069)) {
        if ($text.Contains([string][char]$codePoint, [System.StringComparison]::Ordinal)) {
            return $true
        }
    }

    return $false
}

function Test-ExpectedPseudoWrapper {
    param(
        [Parameter(Mandatory)][string]$Locale,
        [Parameter(Mandatory)][string]$Value
    )

    switch ($Locale) {
        'qps-ploc' { return $Value.StartsWith('[') -and $Value.EndsWith(']') }
        'qps-ploca' { return $Value.StartsWith('[!!_') -and $Value.EndsWith('_!!]') }
        'qps-plocm' { return $Value.StartsWith('[!! ') -and $Value.EndsWith(' !!]') }
        default { return $false }
    }
}

function Test-ResourceSyntax {
    [CmdletBinding()]
    param([string]$File)

    $checkName = 'Test-ResourceSyntax'
    $resolvedFile = Assert-RequiredPath -Path $File -ParameterName 'File'
    try {
        $parsed = Read-LocalizationFile -Path $resolvedFile
        return Complete-CheckBundle -CheckName $checkName -Results @() `
            -PassMessage 'The localization file uses a supported structure and valid UTF-8 text.' -PassFile $parsed.Path
    } catch {
        return New-BlockedBundle -CheckName $checkName -File $resolvedFile -Resource $null `
            -Message $_.Exception.Message `
            -SuggestedAction 'Use a well-formed .resw file or a flat WTA-style .yml file with scalar values only.'
    }
}

function Test-RequiredKeys {
    [CmdletBinding()]
    param(
        [string]$SourceFile,
        [string]$TargetFile,
        [string[]]$Keys
    )

    $checkName = 'Test-RequiredKeys'
    $sourcePath = Assert-RequiredPath -Path $SourceFile -ParameterName 'SourceFile'
    $targetPath = Assert-RequiredPath -Path $TargetFile -ParameterName 'TargetFile'
    $keysWereSupplied = $PSBoundParameters.ContainsKey('Keys')

    try {
        $pair = Read-LocalizationPair -SourceFile $sourcePath -TargetFile $targetPath
        $scope = @(Get-ScopeKeys -SourceEntries $pair.Source.Entries -TargetEntries $pair.Target.Entries -Keys $Keys -KeysWereSupplied $keysWereSupplied)
    } catch [System.ArgumentException] {
        return New-InvalidInputBundle -CheckName $checkName -Message $_.Exception.Message
    } catch {
        return New-BlockedBundle -CheckName $checkName -File $targetPath -Resource $null `
            -Message $_.Exception.Message `
            -SuggestedAction 'Provide comparable supported localization files before checking required keys.'
    }

    $results = [System.Collections.Generic.List[object]]::new()

    foreach ($key in $scope) {
        $sourceHas = $pair.Source.Entries.ContainsKey($key)
        $targetHas = $pair.Target.Entries.ContainsKey($key)

        if ($keysWereSupplied -and -not $sourceHas) {
            $results.Add((New-CheckRecord -Status 'BLOCKED' -CheckName $checkName -File $sourcePath -Resource $key `
                -Expected 'key present in source scope' -Observed 'key missing from source' `
                -Message 'The requested scoped key does not exist in the source file.' `
                -SuggestedAction 'Adjust the supplied -Keys scope to match source entries.'))
            continue
        }

        if ($sourceHas -and -not $targetHas) {
            $results.Add((New-CheckRecord -Status 'FIXABLE' -CheckName $checkName -File $targetPath -Resource $key `
                -Expected 'key present' -Observed 'key missing from target' `
                -Message 'The target file is missing a required entry from the selected scope.' `
                -SuggestedAction 'Add the missing localized entry to the target file.'))
            continue
        }

        if ($targetHas -and -not $sourceHas) {
            $results.Add((New-CheckRecord -Status 'FIXABLE' -CheckName $checkName -File $targetPath -Resource $key `
                -Expected 'key removed' -Observed 'extra key present in target' `
                -Message 'The target file still contains an entry that is outside the selected source scope.' `
                -SuggestedAction 'Remove the stale entry from the target file or widen the caller-supplied scope intentionally.'))
        }
    }

    $passMessage = if ($keysWereSupplied) {
        'All caller-scoped keys are present exactly where expected.'
    } else {
        'The target key set matches the source key set for the whole file.'
    }

    return Complete-CheckBundle -CheckName $checkName -Results @($results.ToArray()) -PassMessage $passMessage -PassFile $targetPath
}

function Test-PlaceholderParity {
    [CmdletBinding()]
    param(
        [string]$SourceFile,
        [string]$TargetFile,
        [string[]]$Keys
    )

    $checkName = 'Test-PlaceholderParity'
    $sourcePath = Assert-RequiredPath -Path $SourceFile -ParameterName 'SourceFile'
    $targetPath = Assert-RequiredPath -Path $TargetFile -ParameterName 'TargetFile'
    $keysWereSupplied = $PSBoundParameters.ContainsKey('Keys')

    try {
        $pair = Read-LocalizationPair -SourceFile $sourcePath -TargetFile $targetPath
        $scope = @(Get-ScopeKeys -SourceEntries $pair.Source.Entries -TargetEntries $pair.Target.Entries -Keys $Keys -KeysWereSupplied $keysWereSupplied -IntersectionWhenUnscoped)
    } catch [System.ArgumentException] {
        return New-InvalidInputBundle -CheckName $checkName -Message $_.Exception.Message
    } catch {
        return New-BlockedBundle -CheckName $checkName -File $targetPath -Resource $null `
            -Message $_.Exception.Message `
            -SuggestedAction 'Provide comparable supported localization files before checking placeholder parity.'
    }

    $results = [System.Collections.Generic.List[object]]::new()

    foreach ($key in $scope) {
        if (-not $pair.Source.Entries.ContainsKey($key) -or -not $pair.Target.Entries.ContainsKey($key)) {
            if ($keysWereSupplied) {
                $results.Add((New-CheckRecord -Status 'BLOCKED' -CheckName $checkName -File $targetPath -Resource $key `
                    -Expected 'matching source and target entries' -Observed 'entry missing from selected scope' `
                    -Message 'Placeholder parity cannot be evaluated because the selected key is missing from one side.' `
                    -SuggestedAction 'Run Test-RequiredKeys first or adjust the supplied -Keys scope.'))
            }
            continue
        }

        $sourceTokens = @(Get-PlaceholderTokens -Value $pair.Source.Entries[$key].Value)
        $targetTokens = @(Get-PlaceholderTokens -Value $pair.Target.Entries[$key].Value)
        $sourceView = Format-TokenMultiset -Tokens $sourceTokens
        $targetView = Format-TokenMultiset -Tokens $targetTokens

        if ($sourceView -cne $targetView) {
            $results.Add((New-CheckRecord -Status 'FIXABLE' -CheckName $checkName -File $targetPath -Resource $key `
                -Expected $sourceView -Observed $targetView `
                -Message 'Placeholder counts or identities do not match the source value.' `
                -SuggestedAction 'Preserve the exact placeholder set, including duplicate occurrences and format specifiers.'))
        }
    }

    return Complete-CheckBundle -CheckName $checkName -Results @($results.ToArray()) `
        -PassMessage 'All comparable entries preserve placeholder identity and count.' -PassFile $targetPath
}

function Test-LockedContent {
    [CmdletBinding()]
    param(
        [string]$SourceFile,
        [string]$TargetFile,
        [string[]]$Keys,
        [string]$Locale
    )

    $checkName = 'Test-LockedContent'
    $sourcePath = Assert-RequiredPath -Path $SourceFile -ParameterName 'SourceFile'
    $targetPath = Assert-RequiredPath -Path $TargetFile -ParameterName 'TargetFile'
    $keysWereSupplied = $PSBoundParameters.ContainsKey('Keys')

    try {
        $pair = Read-LocalizationPair -SourceFile $sourcePath -TargetFile $targetPath
        $scope = @(Get-ScopeKeys -SourceEntries $pair.Source.Entries -TargetEntries $pair.Target.Entries -Keys $Keys -KeysWereSupplied $keysWereSupplied -IntersectionWhenUnscoped)
    } catch [System.ArgumentException] {
        return New-InvalidInputBundle -CheckName $checkName -Message $_.Exception.Message
    } catch {
        return New-BlockedBundle -CheckName $checkName -File $targetPath -Resource $null `
            -Message $_.Exception.Message `
            -SuggestedAction 'Provide comparable supported localization files before checking locked content.'
    }

    $results = [System.Collections.Generic.List[object]]::new()

    foreach ($key in $scope) {
        if (-not $pair.Source.Entries.ContainsKey($key) -or -not $pair.Target.Entries.ContainsKey($key)) {
            if ($keysWereSupplied) {
                $results.Add((New-CheckRecord -Status 'BLOCKED' -CheckName $checkName -File $targetPath -Resource $key `
                    -Expected 'matching source and target entries' -Observed 'entry missing from selected scope' `
                    -Message 'Locked-content validation cannot be evaluated because the selected key is missing from one side.' `
                    -SuggestedAction 'Run Test-RequiredKeys first or adjust the supplied -Keys scope.'))
            }
            continue
        }

        $sourceEntry = $pair.Source.Entries[$key]
        $targetEntry = $pair.Target.Entries[$key]
        $policy = Resolve-LockPolicy -Comments $sourceEntry.Comments -InheritedTokenComments $sourceEntry.InheritedTokenComments -Locale $Locale

        if ($policy.NeedsLocale -and [string]::IsNullOrWhiteSpace($Locale)) {
            $results.Add((New-CheckRecord -Status 'BLOCKED' -CheckName $checkName -File $targetPath -Resource $key `
                -Expected 'locale supplied' -Observed 'locale omitted' `
                -Message 'The source annotations include locale-scoped locks, so this check needs -Locale.' `
                -SuggestedAction 'Rerun the check with the target locale code.'))
            continue
        }

        if (-not [string]::IsNullOrWhiteSpace($policy.BlockedReason)) {
            $results.Add((New-CheckRecord -Status 'BLOCKED' -CheckName $checkName -File $targetPath -Resource $key `
                -Expected 'supported lock scope' -Observed $policy.BlockedReason `
                -Message 'The source annotations use an unsupported file-level lock scope for this check.' `
                -SuggestedAction 'Move the full-lock directive onto the affected entry or quote the specific file-level token(s) to preserve.'))
            continue
        }

        if ($policy.FullLock -and $sourceEntry.Value -cne $targetEntry.Value) {
            $results.Add((New-CheckRecord -Status 'FIXABLE' -CheckName $checkName -File $targetPath -Resource $key `
                -Expected $sourceEntry.Value -Observed $targetEntry.Value `
                -Message 'This entry is locked and must remain identical to the source for the selected locale.' `
                -SuggestedAction 'Restore the exact source value for this entry.'))
            continue
        }

        foreach ($token in $policy.Tokens) {
            if (-not $sourceEntry.Value.Contains($token, [System.StringComparison]::Ordinal)) {
                continue
            }
            if (-not $targetEntry.Value.Contains($token, [System.StringComparison]::Ordinal)) {
                $results.Add((New-CheckRecord -Status 'FIXABLE' -CheckName $checkName -File $targetPath -Resource $key `
                    -Expected $token -Observed $targetEntry.Value `
                    -Message "The locked token '$token' is missing from the target value." `
                    -SuggestedAction 'Reinsert the locked token verbatim.'))
            }
        }
    }

    return Complete-CheckBundle -CheckName $checkName -Results @($results.ToArray()) `
        -PassMessage 'All checked entries preserve source-defined locked content.' -PassFile $targetPath
}

function Test-ResourceEncoding {
    [CmdletBinding()]
    param(
        [string]$File,
        [string]$OriginalFile
    )

    $checkName = 'Test-ResourceEncoding'
    $resolvedFile = Assert-RequiredPath -Path $File -ParameterName 'File'

    $kind = Get-LocalizationFileKind -Path $resolvedFile
    if ($null -eq $kind) {
        return New-BlockedBundle -CheckName $checkName -File $resolvedFile -Resource $null `
            -Message "$resolvedFile uses an unsupported localization format. Supported formats: .resw and WTA-style .yml." `
            -SuggestedAction 'Provide a .resw or flat WTA-style .yml file.'
    }

    $fileBytes = Get-FileBytes -Path $resolvedFile
    try {
        $null = Get-Utf8Text -Bytes $fileBytes -Path $resolvedFile -Kind $kind
    } catch {
        return New-BlockedBundle -CheckName $checkName -File $resolvedFile -Resource $null `
            -Message $_.Exception.Message `
            -SuggestedAction 'Rewrite the file as valid UTF-8 text before validation.'
    }

    $results = [System.Collections.Generic.List[object]]::new()
    if ([string]::IsNullOrWhiteSpace($OriginalFile)) {
        if ($kind -eq 'resw' -and -not (Test-HasUtf8Bom -Bytes $fileBytes)) {
            $results.Add((New-CheckRecord -Status 'FIXABLE' -CheckName $checkName -File $resolvedFile -Resource $null `
                -Expected 'utf-8-bom' `
                -Observed 'utf-8-no-bom' `
                -Message 'New .resw files default to UTF-8 with BOM when no original snapshot is supplied.' `
                -SuggestedAction 'Rewrite the .resw file as UTF-8 with BOM.'))
        }
    } else {
        $originalPath = Assert-RequiredPath -Path $OriginalFile -ParameterName 'OriginalFile'
        $originalKind = Get-LocalizationFileKind -Path $originalPath
        if ($originalKind -ne $kind) {
            return New-BlockedBundle -CheckName $checkName -File $resolvedFile -Resource $null `
                -Message 'OriginalFile and File must use the same supported localization format.' `
                -SuggestedAction 'Compare like-for-like files when checking BOM preservation.'
        }

        $originalBytes = Get-FileBytes -Path $originalPath
        try {
            $null = Get-Utf8Text -Bytes $originalBytes -Path $originalPath -Kind $originalKind
        } catch {
            return New-BlockedBundle -CheckName $checkName -File $originalPath -Resource $null `
                -Message $_.Exception.Message `
                -SuggestedAction 'Provide a valid UTF-8 snapshot file for BOM preservation checks.'
        }

        $expectedBom = Test-HasUtf8Bom -Bytes $originalBytes
        $actualBom = Test-HasUtf8Bom -Bytes $fileBytes
        if ($expectedBom -ne $actualBom) {
            $results.Add((New-CheckRecord -Status 'FIXABLE' -CheckName $checkName -File $resolvedFile -Resource $null `
                -Expected $(if ($expectedBom) { 'utf-8-bom' } else { 'utf-8-no-bom' }) `
                -Observed $(if ($actualBom) { 'utf-8-bom' } else { 'utf-8-no-bom' }) `
                -Message 'The file changed its BOM state relative to the supplied snapshot.' `
                -SuggestedAction 'Restore the snapshot BOM style for this file.'))
        }
    }

    return Complete-CheckBundle -CheckName $checkName -Results @($results.ToArray()) `
        -PassMessage 'The file uses valid UTF-8 and preserves the requested BOM behavior.' -PassFile $resolvedFile
}

function Test-PseudoLocale {
    [CmdletBinding()]
    param(
        [string]$SourceFile,
        [string]$TargetFile,
        [string]$Locale,
        [string[]]$Keys
    )

    $checkName = 'Test-PseudoLocale'
    $sourcePath = Assert-RequiredPath -Path $SourceFile -ParameterName 'SourceFile'
    $targetPath = Assert-RequiredPath -Path $TargetFile -ParameterName 'TargetFile'
    $localeCode = Assert-RequiredValue -Value $Locale -ParameterName 'Locale'
    $keysWereSupplied = $PSBoundParameters.ContainsKey('Keys')

    if ($script:SupportedPseudoLocales -notcontains $localeCode) {
        return New-BlockedBundle -CheckName $checkName -File $targetPath -Resource $null `
            -Message "Pseudo-locale '$localeCode' is unsupported. Supported pseudo-locales: $($script:SupportedPseudoLocales -join ', ')." `
            -SuggestedAction 'Use a supported pseudo-locale code for this focused check.'
    }

    try {
        $pair = Read-LocalizationPair -SourceFile $sourcePath -TargetFile $targetPath
        $scope = @(Get-ScopeKeys -SourceEntries $pair.Source.Entries -TargetEntries $pair.Target.Entries -Keys $Keys -KeysWereSupplied $keysWereSupplied -IntersectionWhenUnscoped)
    } catch [System.ArgumentException] {
        return New-InvalidInputBundle -CheckName $checkName -Message $_.Exception.Message
    } catch {
        return New-BlockedBundle -CheckName $checkName -File $targetPath -Resource $null `
            -Message $_.Exception.Message `
            -SuggestedAction 'Provide comparable supported localization files before checking pseudo-locale quality.'
    }

    $results = [System.Collections.Generic.List[object]]::new()

    foreach ($key in $scope) {
        if (-not $pair.Source.Entries.ContainsKey($key) -or -not $pair.Target.Entries.ContainsKey($key)) {
            if ($keysWereSupplied) {
                $results.Add((New-CheckRecord -Status 'BLOCKED' -CheckName $checkName -File $targetPath -Resource $key `
                    -Expected 'matching source and target entries' -Observed 'entry missing from selected scope' `
                    -Message 'Pseudo-locale validation cannot be evaluated because the selected key is missing from one side.' `
                    -SuggestedAction 'Run Test-RequiredKeys first or adjust the supplied -Keys scope.'))
            }
            continue
        }

        $sourceEntry = $pair.Source.Entries[$key]
        $targetEntry = $pair.Target.Entries[$key]
        $policy = Resolve-LockPolicy -Comments $sourceEntry.Comments -InheritedTokenComments $sourceEntry.InheritedTokenComments -Locale $localeCode

        if (-not [string]::IsNullOrWhiteSpace($policy.BlockedReason)) {
            $results.Add((New-CheckRecord -Status 'BLOCKED' -CheckName $checkName -File $targetPath -Resource $key `
                -Expected 'supported lock scope' -Observed $policy.BlockedReason `
                -Message 'The source annotations use an unsupported file-level lock scope for pseudo-locale validation.' `
                -SuggestedAction 'Move the full-lock directive onto the affected entry or quote the specific file-level token(s) to preserve.'))
            continue
        }

        if ($policy.FullLock) {
            continue
        }

        if ($sourceEntry.Value -ceq $targetEntry.Value) {
            $results.Add((New-CheckRecord -Status 'FIXABLE' -CheckName $checkName -File $targetPath -Resource $key `
                -Expected 'pseudo-localized variant' -Observed $targetEntry.Value `
                -Message 'The pseudo-locale value still matches plain English source text.' `
                -SuggestedAction 'Replace the plain-English fallback with the expected pseudo-localized form.'))
            continue
        }

        $sourceSignal = Get-PseudoLetterSignal -Value $sourceEntry.Value -LockedTokens $policy.Tokens -Locale $localeCode
        if ([string]::IsNullOrWhiteSpace($sourceSignal)) {
            continue
        }

        $targetSignal = Get-PseudoLetterSignal -Value $targetEntry.Value -LockedTokens $policy.Tokens -Locale $localeCode
        if ([string]::IsNullOrWhiteSpace($targetSignal)) {
            $results.Add((New-CheckRecord -Status 'FIXABLE' -CheckName $checkName -File $targetPath -Resource $key `
                -Expected 'transformed pseudo-locale letters' -Observed $targetEntry.Value `
                -Message 'After removing wrappers, placeholders, and locked tokens, the pseudo-locale value has no translatable letter signal left.' `
                -SuggestedAction 'Regenerate this value so its translatable letters still appear in pseudo-localized form.'))
            continue
        }

        $hasDirectionalPseudoSignal = $localeCode -eq 'qps-plocm' -and (Test-ContainsDirectionalPseudoSignal -Value $targetEntry.Value -LockedTokens $policy.Tokens -Locale $localeCode)
        if ($sourceSignal -ceq $targetSignal -and -not $hasDirectionalPseudoSignal) {
            $results.Add((New-CheckRecord -Status 'FIXABLE' -CheckName $checkName -File $targetPath -Resource $key `
                -Expected 'transformed pseudo-locale letters' -Observed $targetEntry.Value `
                -Message 'After removing wrappers, placeholders, and locked tokens, the pseudo-locale value still carries the same plain-English letter signal as the source.' `
                -SuggestedAction 'Change the translatable letters instead of only wrapping, re-casing, or punctuating the English source text.'))
            continue
        }

        if ($pair.Target.Kind -eq 'wta' -and -not (Test-ExpectedPseudoWrapper -Locale $localeCode -Value $targetEntry.Value)) {
            $results.Add((New-CheckRecord -Status 'FIXABLE' -CheckName $checkName -File $targetPath -Resource $key `
                -Expected $localeCode -Observed $targetEntry.Value `
                -Message 'The WTA pseudo-locale value does not use the expected wrapper style.' `
                -SuggestedAction 'Regenerate the pseudo-locale value using the established wrapper style for this locale.'))
        }
    }

    return Complete-CheckBundle -CheckName $checkName -Results @($results.ToArray()) `
        -PassMessage 'Checked pseudo-locale entries avoid plain-English fallback and match applicable style rules.' -PassFile $targetPath
}


function Resolve-TopLevelKeys {
    param(
        [string]$KeysJson,
        [bool]$KeysJsonWasSupplied,
        [string[]]$UnexpectedArguments
    )

    if ($null -ne $UnexpectedArguments -and $UnexpectedArguments.Count -gt 0) {
        $formatted = @($UnexpectedArguments | ForEach-Object { "'$_'" }) -join ', '
        throw [System.ArgumentException]::new("Unexpected positional argument(s): $formatted. Omit KeysJson to scope the whole file.")
    }

    if (-not $KeysJsonWasSupplied) {
        return $null
    }

    if ([string]::IsNullOrWhiteSpace($KeysJson)) {
        throw [System.ArgumentException]::new('KeysJson must be a non-empty JSON array of strings when supplied. Omit KeysJson to scope the whole file.')
    }

    try {
        $parsed = ConvertFrom-Json -InputObject $KeysJson -NoEnumerate -ErrorAction Stop
    } catch {
        throw [System.ArgumentException]::new('KeysJson must be a valid JSON array of strings. Omit KeysJson to scope the whole file.')
    }

    if ($parsed -isnot [System.Array]) {
        throw [System.ArgumentException]::new('KeysJson must decode to a JSON array of strings. Omit KeysJson to scope the whole file.')
    }

    $keys = [System.Collections.Generic.List[string]]::new()
    foreach ($item in @($parsed)) {
        if ($item -isnot [string]) {
            throw [System.ArgumentException]::new('KeysJson must contain only string elements.')
        }
        if ([string]::IsNullOrWhiteSpace($item)) {
            throw [System.ArgumentException]::new('KeysJson must not contain empty strings. Omit KeysJson to scope the whole file.')
        }
        $keys.Add($item)
    }

    if ($keys.Count -eq 0) {
        throw [System.ArgumentException]::new('KeysJson must contain at least one key when supplied. Omit KeysJson to scope the whole file.')
    }

    return @($keys.ToArray())
}

function Invoke-LocalizationCheck {
    [CmdletBinding()]
    param(
        [string]$Check,
        [string]$File,
        [string]$SourceFile,
        [string]$TargetFile,
        [string]$OriginalFile,
        [string]$Locale,
        [string[]]$Keys
    )

    $checkName = Assert-RequiredValue -Value $Check -ParameterName 'Check'
    if ($script:SupportedChecks -notcontains $checkName) {
        throw [System.ArgumentException]::new("Unsupported Check '$checkName'. Supported checks: $($script:SupportedChecks -join ', ').")
    }

    $pairArgs = @{
        SourceFile = $SourceFile
        TargetFile = $TargetFile
    }
    if ($PSBoundParameters.ContainsKey('Keys')) {
        $pairArgs['Keys'] = $Keys
    }

    switch ($checkName) {
        'Test-ResourceSyntax' {
            return Test-ResourceSyntax -File $File
        }
        'Test-RequiredKeys' {
            return Test-RequiredKeys @pairArgs
        }
        'Test-PlaceholderParity' {
            return Test-PlaceholderParity @pairArgs
        }
        'Test-LockedContent' {
            return Test-LockedContent @pairArgs -Locale $Locale
        }
        'Test-ResourceEncoding' {
            return Test-ResourceEncoding -File $File -OriginalFile $OriginalFile
        }
        'Test-PseudoLocale' {
            return Test-PseudoLocale @pairArgs -Locale $Locale
        }
    }
}

function Write-JsonBundleAndExit {
    param([Parameter(Mandatory)]$Bundle)

    [Console]::Out.WriteLine(($Bundle | ConvertTo-Json -Compress -Depth 8))
    exit $Bundle.exitCode
}

function Invoke-LocalizationMain {
    try {
        $cliKeys = Resolve-TopLevelKeys -KeysJson $KeysJson -KeysJsonWasSupplied $script:TopLevelBoundParameters.ContainsKey('KeysJson') -UnexpectedArguments $UnexpectedArguments
        $invokeArgs = @{
            Check = $Check
            File = $File
            SourceFile = $SourceFile
            TargetFile = $TargetFile
            OriginalFile = $OriginalFile
            Locale = $Locale
        }
        if ($script:TopLevelBoundParameters.ContainsKey('KeysJson')) {
            $invokeArgs['Keys'] = $cliKeys
        }

        $bundle = Invoke-LocalizationCheck @invokeArgs
        Write-JsonBundleAndExit -Bundle $bundle
    } catch [System.ArgumentException] {
        Write-JsonBundleAndExit -Bundle (New-InvalidInputBundle -CheckName $Check -Message $_.Exception.Message)
    } catch {
        Write-JsonBundleAndExit -Bundle (New-BlockedBundle -CheckName $(if ($Check) { $Check } else { 'unknown' }) -File $null -Resource $null `
            -Message $_.Exception.Message -SuggestedAction 'Inspect the inputs and rerun the focused file-based check.')
    }
}

if ($MyInvocation.InvocationName -ne '.') {
    Invoke-LocalizationMain
}
