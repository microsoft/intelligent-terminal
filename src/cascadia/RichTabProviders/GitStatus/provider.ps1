# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.

$ErrorActionPreference = 'Stop'

function New-EmptyResponse([string]$RequestId) {
    return @{
        protocolVersion = 1
        requestId = $RequestId
        result = @{ fields = @{} }
    }
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

function Test-LocalTreeWithoutReparsePoint([string]$Root) {
    if (-not (Test-LocalPathWithoutReparsePoint $Root)) {
        return $false
    }

    try {
        $pending = [Collections.Generic.Stack[string]]::new()
        $pending.Push([IO.Path]::GetFullPath($Root))
        while ($pending.Count -gt 0) {
            foreach ($entry in [IO.Directory]::EnumerateFileSystemEntries($pending.Pop())) {
                $attributes = [IO.File]::GetAttributes($entry)
                if (($attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                    return $false
                }
                if (($attributes -band [IO.FileAttributes]::Directory) -ne 0) {
                    $pending.Push($entry)
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
    $request = $requestText | ConvertFrom-Json -Depth 16
    $requestId = [string]$request.requestId
    if ([string]::IsNullOrWhiteSpace($requestId)) {
        throw 'requestId is required'
    }

    $workingDirectory = [string]$request.params.workingDirectory.value
    $authoritative = [bool]$request.params.workingDirectory.authoritative
    $isLocalDrivePath = $workingDirectory -match '^[A-Za-z]:[\\/]'
    $drive = if ($isLocalDrivePath) {
        Get-PSDrive -Name $workingDirectory.Substring(0, 1) -PSProvider FileSystem -ErrorAction SilentlyContinue
    }
    $isRemoteDrive = $drive -and -not [string]::IsNullOrWhiteSpace([string]$drive.DisplayRoot)
    if (-not $authoritative -or
        [string]::IsNullOrWhiteSpace($workingDirectory) -or
        -not $isLocalDrivePath -or
        -not $drive -or
        $isRemoteDrive -or
        -not (Test-Path -LiteralPath $workingDirectory -PathType Container) -or
        -not (Test-LocalPathWithoutReparsePoint $workingDirectory)) {
        $response = New-EmptyResponse $requestId
    }
    else {
        $repository = Find-LocalGitRepository $workingDirectory
        if (-not $repository) {
            $response = New-EmptyResponse $requestId
        }
        else {
            $root = [string]$repository.Root
            $gitDirectory = [string]$repository.GitDirectory
            $configPath = Join-Path $gitDirectory 'config'
            $worktreeConfigPath = Join-Path $gitDirectory 'config.worktree'
            $commonDirectoryPath = Join-Path $gitDirectory 'commondir'
            $alternatesPath = Join-Path $gitDirectory 'objects\info\alternates'
            $objectsPath = Join-Path $gitDirectory 'objects'
            $infoPath = Join-Path $gitDirectory 'info'
            $config = Get-Item -LiteralPath $configPath -Force -ErrorAction SilentlyContinue
            if (($config -and $config.Length -gt 1MB) -or
                -not (Test-LocalTreeWithoutReparsePoint $gitDirectory) -or
                ($config -and -not (Test-LocalPathWithoutReparsePoint $configPath)) -or
                (Test-Path -LiteralPath $commonDirectoryPath) -or
                (Test-Path -LiteralPath $worktreeConfigPath) -or
                (Test-Path -LiteralPath $alternatesPath) -or
                -not (Test-LocalPathWithoutReparsePoint $objectsPath) -or
                ((Test-Path -LiteralPath $infoPath) -and
                    -not (Test-LocalPathWithoutReparsePoint $infoPath))) {
                $response = New-EmptyResponse $requestId
            }
            else {
                $configText = if ($config) {
                    [IO.File]::ReadAllText($config.FullName)
                }
                if ($configText -match '(?im)^\s*\[\s*include(?:if)?(?:\s|\])' -or
                    $configText -match '(?im)^\s*worktreeconfig\s*=') {
                    $response = New-EmptyResponse $requestId
                }
                else {
                    $git = Get-Command git.exe -CommandType Application -ErrorAction SilentlyContinue
                    if (-not $git) {
                        $response = New-EmptyResponse $requestId
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
                        $env:GIT_CONFIG_GLOBAL = 'NUL'
                        $env:GIT_CONFIG_NOSYSTEM = '1'
                        $env:GIT_CONFIG_SYSTEM = 'NUL'
                        $env:GIT_NO_LAZY_FETCH = '1'
                        $env:GIT_PROTOCOL_FROM_USER = '0'
                        $env:GIT_TERMINAL_PROMPT = '0'

                        $gitOptions = @(
                            '--no-optional-locks'
                            '-c'
                            'core.fsmonitor=false'
                            '-c'
                            'core.attributesFile=NUL'
                            '-c'
                            'core.excludesFile=NUL'
                            '-c'
                            "core.hooksPath=$PSScriptRoot"
                            "--git-dir=$gitDirectory"
                            "--work-tree=$root"
                        )
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
                        $dirty = $false
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
                                $dirty = $true
                            }
                        }

                        if ([string]::IsNullOrWhiteSpace($branch) -or $branch -eq '(detached)') {
                            $branch = if ($oid.Length -gt 8) { $oid.Substring(0, 8) } else { $oid }
                        }
                        $repositoryName = Split-Path -Leaf $root
                        $status = if ($dirty) { 'dirty' } else { 'clean' }
                        $syncParts = @()
                        if ($ahead -gt 0) {
                            $syncParts += "ahead $ahead"
                        }
                        if ($behind -gt 0) {
                            $syncParts += "behind $behind"
                        }

                        $fields = @{
                            repository = $repositoryName
                            branch = $branch
                        }
                        if ($filterDrivers.Count -eq 0) {
                            $fields.status = $status
                        }
                        if ($syncParts.Count -gt 0) {
                            $fields.sync = $syncParts -join ', '
                        }

                        $tooltip = @($root, "branch: $branch")
                        if ($filterDrivers.Count -eq 0) {
                            $tooltip += "status: $status"
                        }
                        if (-not [string]::IsNullOrWhiteSpace($upstream)) {
                            $tooltip += "upstream: $upstream"
                        }
                        if ($syncParts.Count -gt 0) {
                            $tooltip += $syncParts -join ', '
                        }

                        $accessibility = "$repositoryName repository, branch $branch"
                        if ($filterDrivers.Count -eq 0) {
                            $accessibility += ", $status"
                        }
                        if ($syncParts.Count -gt 0) {
                            $accessibility += ", " + ($syncParts -join ', ')
                        }

                        $response = @{
                            protocolVersion = 1
                            requestId = $requestId
                            result = @{
                                fields = $fields
                                tooltip = $tooltip -join "`n"
                                accessibilityText = $accessibility
                            }
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
    exit 1
}
