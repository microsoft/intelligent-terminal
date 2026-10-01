# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.

$utf8NoBom = New-Object System.Text.UTF8Encoding $false
[Console]::InputEncoding = $utf8NoBom
[Console]::OutputEncoding = $utf8NoBom
$OutputEncoding = $utf8NoBom

$ErrorActionPreference = 'Stop'

function New-EmptyResponse([string]$RequestId, [hashtable]$Fields) {
    return @{
        protocolVersion = 1
        requestId = $RequestId
        result = @{ fields = $Fields }
    }
}

function Format-LabeledValue([string]$Label, [string]$Value) {
    if ([string]::IsNullOrWhiteSpace($Label)) {
        return $Value
    }
    return "${Label}: $Value"
}

function Test-LocalPathWithoutReparsePoint([string]$Path) {
    try {
        $fullPath = [IO.Path]::GetFullPath($Path)
        $root = [IO.Path]::GetPathRoot($fullPath)
        if ([string]::IsNullOrWhiteSpace($root)) {
            return $false
        }

        $current = $root
        $relative = $fullPath.Substring($root.Length)
        foreach ($component in $relative.Split(
            [char[]]@('\', '/'),
            [StringSplitOptions]::RemoveEmptyEntries)) {
            $current = Join-Path $current $component
            $item = Get-Item -LiteralPath $current -Force -ErrorAction Stop
            if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                return $false
            }
        }
        return $true
    }
    catch {
        return $false
    }
}

function Test-ImmediateChildrenWithoutReparsePoint([string]$Path, [int]$MaximumEntries) {
    try {
        if (-not (Test-Path -LiteralPath $Path)) {
            return $true
        }

        $count = 0
        foreach ($item in Get-ChildItem -LiteralPath $Path -Force -ErrorAction Stop) {
            $count++
            if ($count -gt $MaximumEntries -or
                ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                return $false
            }
        }
        return $true
    }
    catch {
        return $false
    }
}

function Test-BoundedTreeWithoutReparsePoint([string]$Path, [int]$MaximumEntries) {
    try {
        if (-not (Test-Path -LiteralPath $Path)) {
            return $true
        }

        $pending = New-Object System.Collections.Stack
        $pending.Push($Path)
        $count = 0
        while ($pending.Count -gt 0) {
            $current = [string]$pending.Pop()
            $currentItem = Get-Item -LiteralPath $current -Force -ErrorAction Stop
            if (($currentItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                return $false
            }
            foreach ($item in Get-ChildItem -LiteralPath $current -Force -ErrorAction Stop) {
                $count++
                if ($count -gt $MaximumEntries -or
                    ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                    return $false
                }
                if ($item.PSIsContainer) {
                    $pending.Push($item.FullName)
                }
            }
        }
        return $true
    }
    catch {
        return $false
    }
}

function Find-LocalGitRepository([string]$WorkingDirectory) {
    $current = Get-Item -LiteralPath $WorkingDirectory -Force -ErrorAction SilentlyContinue
    while ($current -and $current.PSIsContainer) {
        $gitPath = Join-Path $current.FullName '.git'
        $gitItem = Get-Item -LiteralPath $gitPath -Force -ErrorAction SilentlyContinue
        if ($gitItem) {
            if (-not $gitItem.PSIsContainer -or
                ($gitItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                return $null
            }
            return @{
                Root = $current.FullName
                GitDirectory = $gitItem.FullName
            }
        }
        $current = $current.Parent
    }
    return $null
}

try {
    $requestText = [Console]::In.ReadToEnd()
    $request = $requestText | ConvertFrom-Json
    $requestId = [string]$request.requestId
    if ([string]::IsNullOrWhiteSpace($requestId)) {
        throw 'requestId is required'
    }

    $workingDirectory = [string]$request.params.workingDirectory.value
    $authoritative = [bool]$request.params.workingDirectory.authoritative
    $baseFields = @{}
    if ($authoritative -and -not [string]::IsNullOrWhiteSpace($workingDirectory)) {
        $baseFields.workingDirectory = $workingDirectory
    }
    $localizedLabels = @{}
    $allowedFirstPartyFields = @('agentStatus')
    $allowedLocalizedLabels = @('branchLabel', 'changesLabel')
    foreach ($property in $request.params.firstPartyFields.PSObject.Properties) {
        if ($allowedFirstPartyFields -contains $property.Name -and
            -not [string]::IsNullOrWhiteSpace([string]$property.Value)) {
            $baseFields[$property.Name] = [string]$property.Value
        }
        elseif ($allowedLocalizedLabels -contains $property.Name -and
                -not [string]::IsNullOrWhiteSpace([string]$property.Value)) {
            $localizedLabels[$property.Name] = [string]$property.Value
        }
    }

    $isLocalDrivePath = $workingDirectory -match '^[A-Za-z]:[\\/]'
    $drive = if ($isLocalDrivePath) {
        Get-PSDrive -Name $workingDirectory.Substring(0, 1) -PSProvider FileSystem -ErrorAction SilentlyContinue
    }
    $isRemoteDrive = $drive -and -not [string]::IsNullOrWhiteSpace([string]$drive.DisplayRoot)

    $visibleFields = @()
    if ($request.params.visibleFields) {
        $visibleFields = @($request.params.visibleFields)
    }
    $gitFields = @('repository', 'branch', 'changes')
    $needsGit = $false
    if ($visibleFields.Count -eq 0) {
        $needsGit = $true
    }
    else {
        foreach ($gf in $gitFields) {
            if ($visibleFields -contains $gf) {
                $needsGit = $true
                break
            }
        }
    }

    if (-not $authoritative -or
        [string]::IsNullOrWhiteSpace($workingDirectory) -or
        -not $isLocalDrivePath -or
        -not $drive -or
        $isRemoteDrive -or
        -not (Test-Path -LiteralPath $workingDirectory -PathType Container) -or
        -not (Test-LocalPathWithoutReparsePoint $workingDirectory) -or
        -not $needsGit) {
        $response = New-EmptyResponse $requestId $baseFields
    }
    else {
        $repository = Find-LocalGitRepository $workingDirectory
        if (-not $repository) {
            $response = New-EmptyResponse $requestId $baseFields
        }
        else {
            $root = [string]$repository.Root
            $gitDirectory = [string]$repository.GitDirectory
            $configPath = Join-Path $gitDirectory 'config'
            $worktreeConfigPath = Join-Path $gitDirectory 'config.worktree'
            $commonDirectoryPath = Join-Path $gitDirectory 'commondir'
            $alternatesPath = Join-Path $gitDirectory 'objects\info\alternates'
            $objectsPath = Join-Path $gitDirectory 'objects'
            $objectPackPath = Join-Path $objectsPath 'pack'
            $objectInfoPath = Join-Path $objectsPath 'info'
            $infoPath = Join-Path $gitDirectory 'info'
            $headPath = Join-Path $gitDirectory 'HEAD'
            $indexPath = Join-Path $gitDirectory 'index'
            $packedRefsPath = Join-Path $gitDirectory 'packed-refs'
            $refsPath = Join-Path $gitDirectory 'refs'
            $logsPath = Join-Path $gitDirectory 'logs'
            $config = Get-Item -LiteralPath $configPath -Force -ErrorAction SilentlyContinue
            $gitAccessPaths = @(
                $gitDirectory
                $configPath
                $headPath
                $indexPath
                $packedRefsPath
                $refsPath
                $logsPath
                $objectsPath
                $infoPath
            )
            $unsafeGitAccessPath = $gitAccessPaths |
                Where-Object {
                    (Test-Path -LiteralPath $_) -and
                    -not (Test-LocalPathWithoutReparsePoint $_)
                } |
                Select-Object -First 1
            $unsafeGitTraversal =
                -not (Test-ImmediateChildrenWithoutReparsePoint $objectsPath 512) -or
                -not (Test-BoundedTreeWithoutReparsePoint $objectPackPath 8192) -or
                -not (Test-BoundedTreeWithoutReparsePoint $objectInfoPath 1024) -or
                -not (Test-BoundedTreeWithoutReparsePoint $refsPath 32768) -or
                -not (Test-BoundedTreeWithoutReparsePoint $logsPath 32768)
            if (($config -and $config.Length -gt 1MB) -or
                $unsafeGitAccessPath -or
                $unsafeGitTraversal -or
                (Test-Path -LiteralPath $commonDirectoryPath) -or
                (Test-Path -LiteralPath $worktreeConfigPath) -or
                (Test-Path -LiteralPath $alternatesPath)) {
                $response = New-EmptyResponse $requestId $baseFields
            }
            else {
                $configText = if ($config) {
                    [IO.File]::ReadAllText($config.FullName)
                }
                if ($configText -match '(?im)^\s*\[\s*include(?:if)?(?:\s|\])' -or
                    $configText -match '(?im)^\s*worktreeconfig\s*=') {
                    $response = New-EmptyResponse $requestId $baseFields
                }
                else {
                    $gitBinary = if (-not [string]::IsNullOrWhiteSpace($env:INTELLIGENT_TERMINAL_GIT_BINARY)) {
                        $env:INTELLIGENT_TERMINAL_GIT_BINARY
                    }
                    elseif ($request.params.firstPartyFields -and
                            $request.params.firstPartyFields.PSObject.Properties['gitBinary'] -and
                            -not [string]::IsNullOrWhiteSpace([string]$request.params.firstPartyFields.gitBinary)) {
                        [string]$request.params.firstPartyFields.gitBinary
                    }
                    else {
                        'git.exe'
                    }

                    $git = Get-Command $gitBinary -CommandType Application -ErrorAction SilentlyContinue
                    if (-not $git) {
                        $response = New-EmptyResponse $requestId $baseFields
                    }
                    else {
                        $gitEnvironmentVariables = @(
                            'GIT_ALTERNATE_OBJECT_DIRECTORIES',
                            'GIT_ATTR_SOURCE',
                            'GIT_COMMON_DIR',
                            'GIT_CONFIG_PARAMETERS',
                            'GIT_DIR',
                            'GIT_EXTERNAL_DIFF',
                            'GIT_INDEX_FILE',
                            'GIT_OBJECT_DIRECTORY',
                            'GIT_WORK_TREE'
                        )
                        foreach ($name in $gitEnvironmentVariables) {
                            Remove-Item "Env:$name" -ErrorAction SilentlyContinue
                        }
                        $env:GIT_ATTR_NOSYSTEM = '1'
                        $env:GIT_CONFIG_COUNT = '0'
                        $env:GIT_CONFIG_GLOBAL = '/dev/null'
                        $env:GIT_CONFIG_NOSYSTEM = '1'
                        $env:GIT_CONFIG_SYSTEM = '/dev/null'
                        $env:GIT_NO_LAZY_FETCH = '1'
                        $env:GIT_PROTOCOL_FROM_USER = '0'
                        $env:GIT_TERMINAL_PROMPT = '0'

                        $gitOptions = @(
                            '--no-optional-locks'
                            '-c'
                            'core.fsmonitor=false'
                            '-c'
                            'core.attributesFile='
                            '-c'
                            'core.excludesFile='
                            '-c'
                            "core.hooksPath=$PSScriptRoot"
                            "--git-dir=$gitDirectory"
                            "--work-tree=$root"
                        )
                        try {
                            $localAutoCrlf = @(
                                & $git.Source @gitOptions config --local --get core.autocrlf 2>$null
                            ) | Select-Object -Last 1
                            $autoCrlfExitCode = $LASTEXITCODE
                            if ($autoCrlfExitCode -ne 0 -and $autoCrlfExitCode -ne 1) {
                                throw 'git core.autocrlf inspection failed'
                            }
                            if ([string]$localAutoCrlf -notmatch '^(?:true|false|input)$') {
                                $localAutoCrlf = 'true'
                            }
                            $gitOptions += @('-c', "core.autocrlf=$localAutoCrlf")
                            $filters = @(
                                & $git.Source @gitOptions config --local --get-regexp `
                                    '^filter\..*\.(clean|process)$' 2>$null
                            )
                            $filterExitCode = $LASTEXITCODE
                            if ($filterExitCode -ne 0 -and $filterExitCode -ne 1) {
                                throw 'git config inspection failed'
                            }
                            $filterDrivers = @(
                                foreach ($filter in $filters) {
                                    if ([string]$filter -match '^filter\.(.+)\.(?:clean|process)\s') {
                                        $Matches[1]
                                    }
                                }
                            ) | Sort-Object -Unique
                            foreach ($driver in $filterDrivers) {
                                $gitOptions += @(
                                    '-c', "filter.$driver.clean="
                                    '-c', "filter.$driver.process="
                                    '-c', "filter.$driver.required=false"
                                )
                            }

                            $lines = @(
                                & $git.Source @gitOptions status `
                                    --porcelain=v2 --branch --untracked-files=normal --ignore-submodules=all 2>$null
                            )
                            if ($LASTEXITCODE -ne 0) {
                                throw 'git status failed'
                            }

                            $branch = ''
                            $oid = ''
                            $upstream = ''
                            $ahead = 0
                            $behind = 0
                            $changedFileCount = 0
                            foreach ($line in $lines) {
                                $text = [string]$line
                                if ($text.StartsWith('# branch.head ')) {
                                    $branch = $text.Substring(14)
                                }
                                elseif ($text.StartsWith('# branch.oid ')) {
                                    $oid = $text.Substring(13)
                                }
                                elseif ($text.StartsWith('# branch.upstream ')) {
                                    $upstream = $text.Substring(18)
                                }
                                elseif ($text -match '^# branch\.ab \+(\d+) -(\d+)$') {
                                    $ahead = [int]$Matches[1]
                                    $behind = [int]$Matches[2]
                                }
                                elseif (-not $text.StartsWith('# ')) {
                                    $changedFileCount++
                                }
                            }

                            if ([string]::IsNullOrWhiteSpace($branch) -or $branch -eq '(detached)') {
                                $branch = if ($oid.Length -gt 8) { $oid.Substring(0, 8) } else { $oid }
                            }
                            $repositoryName = Split-Path -Leaf $root

                            & $git.Source @gitOptions rev-parse --verify --quiet HEAD 2>$null | Out-Null
                            $headExitCode = $LASTEXITCODE
                            if ($headExitCode -eq 0) {
                                $numstatLines = @(
                                    & $git.Source @gitOptions diff `
                                        --numstat --no-renames --no-ext-diff --no-textconv HEAD -- 2>$null
                                )
                            }
                            elseif ($headExitCode -eq 1 -or $headExitCode -eq 128) {
                                $cachedNumstatLines = @(
                                    & $git.Source @gitOptions diff `
                                        --cached --numstat --no-renames --no-ext-diff --no-textconv -- 2>$null
                                )
                                $cachedNumstatExitCode = $LASTEXITCODE
                                $unstagedNumstatLines = @(
                                    & $git.Source @gitOptions diff `
                                        --numstat --no-renames --no-ext-diff --no-textconv -- 2>$null
                                )
                                $unstagedNumstatExitCode = $LASTEXITCODE
                                if ($cachedNumstatExitCode -ne 0 -or $unstagedNumstatExitCode -ne 0) {
                                    throw 'git numstat failed'
                                }
                                $numstatLines = @($cachedNumstatLines) + @($unstagedNumstatLines)
                            }
                            else {
                                throw 'git HEAD inspection failed'
                            }
                            if ($headExitCode -eq 0 -and $LASTEXITCODE -ne 0) {
                                throw 'git numstat failed'
                            }

                            $additions = 0
                            $deletions = 0
                            foreach ($line in $numstatLines) {
                                if ([string]$line -match '^(\d+|-)\s+(\d+|-)\s+') {
                                    if ($Matches[1] -ne '-') {
                                        $additions += [int]$Matches[1]
                                    }
                                    if ($Matches[2] -ne '-') {
                                        $deletions += [int]$Matches[2]
                                    }
                                }
                            }

                            $fields = @{}
                            foreach ($entry in $baseFields.GetEnumerator()) {
                                $fields[$entry.Key] = $entry.Value
                            }
                            $fields.repository = $repositoryName
                            $fields.branch = $branch
                            $fields.changes = "~$changedFileCount +$additions -$deletions"
                            $changesValue = $fields.changes
                            $tooltip = @(
                                $root
                                (Format-LabeledValue $localizedLabels.branchLabel $branch)
                                (Format-LabeledValue $localizedLabels.changesLabel $changesValue)
                            )
                            if (-not [string]::IsNullOrWhiteSpace($upstream)) {
                                $tooltip += $upstream
                                if ($ahead -gt 0) {
                                    $tooltip += "$branch --$ahead--> $upstream"
                                }
                                if ($behind -gt 0) {
                                    $tooltip += "$upstream --$behind--> $branch"
                                }
                            }

                            $response = @{
                                protocolVersion = 1
                                requestId = $requestId
                                result = @{
                                    fields = $fields
                                    tooltip = $tooltip -join "`n"
                                }
                            }
                        }
                        catch {
                            [Console]::Error.WriteLine("Git status inspection failed: $($_.Exception.Message)")
                            $response = New-EmptyResponse $requestId $baseFields
                        }
                    }
                }
            }
        }
    }

    [Console]::Out.Write(($response | ConvertTo-Json -Compress -Depth 16))
}
catch {
    [Console]::Error.WriteLine($_.Exception.Message)
    if (-not [string]::IsNullOrWhiteSpace($requestId) -and $baseFields) {
        $fallbackResponse = New-EmptyResponse $requestId $baseFields
        [Console]::Out.Write(($fallbackResponse | ConvertTo-Json -Compress -Depth 16))
        exit 0
    }
    exit 1
}
