param(
    [Parameter(Mandatory)][string]$IntegrationPath,
    [Parameter(Mandatory)]
    [ValidateSet('HostGate', 'FirstStatus', 'Lifecycle', 'LifecycleMissingFallback', 'AdoptedLegacyUpgrade', 'LaterReaderUpgrade', 'StrictLegacyUpgrade', 'StrictFalseInstalledInitialization', 'StrictFalseInstalledReadlineInitialization', 'Chaining', 'WorkChangingChain', 'Throwing', 'FailSafe', 'Constrained')]
    [string]$Scenario
)

$ErrorActionPreference = 'Stop'
$Global:Assertions = 0
$Global:BaseCalls = 0
$Global:ChainCalls = 0
$Global:ReadCalls = 0
$Global:SeenStatus = $null
$env:INTELLIGENT_TERMINAL = '1'

function Assert-Equal($Actual, $Expected, [string]$Because) {
    $Global:Assertions++
    if ($Actual -cne $Expected) {
        throw "${Because}: expected <$Expected>, got <$Actual>"
    }
}

function Assert-Render([string]$Output, [string]$Text, [int]$Finished = 0, [int]$ExitCode = 0) {
    $esc = [char]27
    $bel = [char]7
    Assert-Equal ([regex]::Matches($Output, [regex]::Escape("$esc]133;A$bel")).Count) 1 'one prompt-start mark'
    Assert-Equal ([regex]::Matches($Output, [regex]::Escape("$esc]133;B$bel")).Count) 1 'one prompt-end mark'
    Assert-Equal ([regex]::Matches($Output, [regex]::Escape("$esc]9;9;")).Count) 1 'one cwd mark'
    Assert-Equal ([regex]::Matches($Output, [regex]::Escape("$esc]9001;ShellType;")).Count) 1 'one shell identity mark'
    Assert-Equal ([regex]::Matches($Output, [regex]::Escape("$esc]133;D;")).Count) $Finished 'command-finished count'
    if ($Finished) {
        Assert-Equal ($Output.StartsWith("$esc]133;D;$ExitCode$bel$esc]133;A$bel")) $true 'finished status precedes prompt'
    }
    else {
        Assert-Equal ($Output.StartsWith("$esc]133;A$bel")) $true 'prompt-start is first without a completed command'
    }
    $visible = [regex]::Replace($Output, "$esc\][^$bel]*$bel", '')
    Assert-Equal $visible $Text 'visible delegated prompt'
    Assert-Equal ($Output.EndsWith("$esc]133;B$bel")) $true 'input boundary is last'
}

function Global:prompt {
    $Global:SeenStatus = $?
    $Global:BaseCalls++
    'BASE> '
}

if ($Scenario -ne 'StrictFalseInstalledInitialization') {
    function Global:PSConsoleHostReadLine {
        $Global:ReadCalls++
        'fixture command'
    }
}

function New-ChainedPrompt([string]$Label, [scriptblock]$Saved) {
    {
        $status = $?
        $Global:ChainCalls++
        if ($status) { $null = $true }
        else { Get-Variable '__Lifecycle_Missing__' -ErrorAction Ignore }
        $text = & $Saved
        "${Label}:${text}"
    }.GetNewClosure()
}

function New-WorkChangingPrompt([scriptblock]$Saved) {
    {
        $Global:ChainCalls++
        $text = & $Saved
        "work:${text}"
    }.GetNewClosure()
}

function Invoke-ReadBoundary {
    $before = $Global:ReadCalls
    $line = PSConsoleHostReadLine
    Assert-Equal $line 'fixture command' 'readline output is preserved'
    Assert-Equal $Global:ReadCalls ($before + 1) 'original readline called once'
}

try {
    if ($Scenario -eq 'WorkChangingChain') {
        $basePrompt = $function:prompt
        Set-Item Function:\global:prompt (New-WorkChangingPrompt $basePrompt)
        $Global:LASTEXITCODE = 17
        Get-Variable '__Lifecycle_Missing__' -ErrorAction Ignore
        $controlText = prompt
        $control = @{
            Text = $controlText; Status = $Global:SeenStatus
            BaseCalls = $Global:BaseCalls; ChainCalls = $Global:ChainCalls
            ExitCode = $Global:LASTEXITCODE
        }
        Assert-Equal $control.Status $true 'ordinary successful chain work changes failure to success without integration'
        Assert-Equal $control.Text 'work:BASE> ' 'control invokes equivalent saved base'
        Set-Item Function:\global:prompt $basePrompt
        $Global:BaseCalls = 0
        $Global:ChainCalls = 0
    }
    if ($Scenario -eq 'HostGate') {
        $env:INTELLIGENT_TERMINAL = '0'
        . $IntegrationPath
        $output = prompt
        Assert-Equal $output 'BASE> ' 'non-product host receives no integration marks'
        Assert-Equal $Global:__ShellInteg_Installed $null 'non-product host does not install integration'
        @{ Scenario = $Scenario; Version = "$($PSVersionTable.PSVersion)"; Assertions = $Global:Assertions } |
            ConvertTo-Json -Compress
        exit 0
    }
    if ($Scenario -eq 'Constrained') {
        $ExecutionContext.SessionState.LanguageMode = 'ConstrainedLanguage'
    }
    if ($Scenario -in @('Lifecycle', 'LifecycleMissingFallback', 'AdoptedLegacyUpgrade', 'LaterReaderUpgrade', 'StrictLegacyUpgrade')) {
        # Model v14's shared-state delegation and adoption, including its
        # initially empty fallback and absent v15 migration sentinel.
        $Global:__ShellInteg_OriginalPrompt = $function:prompt
        $Global:__ShellInteg_PrevPrompt = $null
        $Global:__ShellInteg_Rendering = $false
        $Global:__ShellInteg_OriginalPSConsoleHostReadLine = $function:PSConsoleHostReadLine
        $Global:__ShellInteg_CanInspectErrors = $true
        $Global:__ShellInteg_Installed = $true
        function Global:prompt {
            if ($Global:__ShellInteg_Rendering) {
                if ($Global:__ShellInteg_PrevPrompt) {
                    return (& $Global:__ShellInteg_PrevPrompt)
                }
                return ''
            }
            $Global:__ShellInteg_Rendering = $true
            try { & $Global:__ShellInteg_OriginalPrompt }
            finally { $Global:__ShellInteg_Rendering = $false }
        }
        $Global:__ShellInteg_Wrapper = $function:prompt
        $legacyWrapper = $function:prompt
        function Global:__ShellInteg_Rearm {
            $current = $function:prompt
            if ($current -eq $Global:__ShellInteg_Wrapper) { return }
            if ($current -ne $Global:__ShellInteg_OriginalPrompt) {
                $Global:__ShellInteg_PrevPrompt = $Global:__ShellInteg_OriginalPrompt
                $Global:__ShellInteg_OriginalPrompt = $current
            }
            Set-Item Function:\global:prompt $Global:__ShellInteg_Wrapper
        }
        function Global:PSConsoleHostReadLine {
            __ShellInteg_Rearm
            $line = & $Global:__ShellInteg_OriginalPSConsoleHostReadLine @args
            $Global:__ShellInteg_LastSubmittedLine =
                if ($line -is [string]) { $line } else { $null }
            return $line
        }
        if ($Scenario -eq 'LaterReaderUpgrade') {
            $savedCore = $Global:__ShellInteg_OriginalPSConsoleHostReadLine
            $Global:ReaderPreCalls = 0
            $Global:ReaderPostCalls = 0
            $readerModule = New-Module -Name LaterReaderModule -ArgumentList $function:PSConsoleHostReadLine -ScriptBlock {
                param([scriptblock]$SavedReader)
                $script:SavedReader = $SavedReader
                function PSConsoleHostReadLine {
                    $Global:ReaderPreCalls++
                    if ($Global:ReaderPreCalls -gt 10) { throw 'fixture reader recursion fuse' }
                    $line = & $script:SavedReader @args
                    $Global:ReaderPostCalls++
                    return $line
                }
                Export-ModuleMember -Function PSConsoleHostReadLine
            }
            Import-Module $readerModule -Global
            $activeReader = $function:PSConsoleHostReadLine
            Assert-Equal (Get-Command PSConsoleHostReadLine).ModuleName 'LaterReaderModule' 'real later module owns reader before upgrade'
        }
        if ($Scenario -eq 'LifecycleMissingFallback') {
            # Retain coverage of older installed state without a fallback
            # variable; Lifecycle retains v14's normal initialized null value.
            Remove-Variable __ShellInteg_PrevPrompt -Scope Global
        }
    }
    if ($Scenario -eq 'AdoptedLegacyUpgrade') {
        $saved = $function:prompt
        $renderer = {
            $Global:ChainCalls++
            # Bound a broken migration's recursion without modifying the embed.
            if ($Global:ChainCalls -gt 12) { throw 'fixture recursion fuse' }
            $text = & $saved
            "legacy:${text}"
        }.GetNewClosure()
        Set-Item Function:\global:prompt $renderer
        Invoke-ReadBoundary
        Assert-Equal ($Global:__ShellInteg_OriginalPrompt -eq $renderer) $true 'v14 adopted chain before migration'
        Assert-Equal (prompt) 'legacy:BASE> ' 'legacy chain works before migration'
        Assert-Equal $Global:BaseCalls 1 'legacy base called once before migration'
        Assert-Equal $Global:ChainCalls 1 'legacy chain called once before migration'
        $fallback = $Global:__ShellInteg_PrevPrompt
        $Global:BaseCalls = 0
        $Global:ChainCalls = 0
    }
    if ($Scenario -in @('StrictFalseInstalledInitialization', 'StrictFalseInstalledReadlineInitialization')) {
        Assert-Equal (Test-Path Function:\PSConsoleHostReadLine) ($Scenario -eq 'StrictFalseInstalledReadlineInitialization') 'expected readline presence before initialization'
        $readline = $function:PSConsoleHostReadLine
        $Global:__ShellInteg_Installed = $false
        Assert-Equal (@(Get-Variable __ShellInteg_* -Scope Global).Count) 1 'only the false Installed guard is initialized'
        foreach ($name in @('__ShellInteg_OriginalPrompt', '__ShellInteg_Wrapper', '__ShellInteg_WrapperTemplate', '__ShellInteg_OriginalPSConsoleHostReadLine')) {
            Assert-Equal ([bool](Get-Variable $name -Scope Global -ErrorAction Ignore)) $false "$name is absent before initialization"
        }
        try {
            Set-StrictMode -Version 2.0
            . $IntegrationPath
        }
        finally { Set-StrictMode -Off }
    }
    elseif ($Scenario -eq 'StrictLegacyUpgrade') {
        Assert-Equal ([bool](Get-Variable __ShellInteg_WrapperTemplate -Scope Global -ErrorAction Ignore)) $false 'v14 has no migration sentinel'
        try {
            Set-StrictMode -Version 2.0
            . $IntegrationPath
        }
        finally { Set-StrictMode -Off }
    }
    else { . $IntegrationPath }
    $first = prompt
    $expected = if ($Scenario -eq 'AdoptedLegacyUpgrade') { 'legacy:BASE> ' } else { 'BASE> ' }
    Assert-Render $first $expected
    Assert-Equal $Global:BaseCalls 1 'first prompt delegates once'

    switch ($Scenario) {
        LaterReaderUpgrade {
            foreach ($stage in @('Upgrade', 'RepeatSource1', 'RepeatSource2')) {
                if ($stage -ne 'Upgrade') { . $IntegrationPath }
                Assert-Equal ([object]::ReferenceEquals($function:PSConsoleHostReadLine, $activeReader)) $true "$stage preserves active reader identity"
                Assert-Equal ([object]::ReferenceEquals((Get-Command PSConsoleHostReadLine).Module, $readerModule)) $true "$stage preserves module ownership"
                Assert-Equal ([object]::ReferenceEquals($Global:__ShellInteg_OriginalPSConsoleHostReadLine, $savedCore)) $true "$stage preserves original core delegate"
                function Global:prompt { 'REPLACEMENT> ' }
                $beforePre = $Global:ReaderPreCalls
                $beforePost = $Global:ReaderPostCalls
                Invoke-ReadBoundary
                Assert-Equal $Global:ReaderPreCalls ($beforePre + 1) "$stage invokes module pre callback once"
                Assert-Equal $Global:ReaderPostCalls ($beforePost + 1) "$stage invokes module post callback once"
                Assert-Equal $Global:__ShellInteg_LastSubmittedLine 'fixture command' "$stage preserves v14 submitted-line tracking"
                Assert-Equal ($function:prompt -eq $Global:__ShellInteg_Wrapper) $true "$stage rearms prompt through saved v14 reader"
                Assert-Render (prompt) 'REPLACEMENT> '
            }
        }
        StrictFalseInstalledInitialization {
            . $IntegrationPath
            Assert-Render (prompt) 'BASE> '
            Assert-Equal $Global:BaseCalls 2 'repeat source delegates once per render after false-guard initialization'
        }
        StrictFalseInstalledReadlineInitialization {
            Assert-Equal ($Global:__ShellInteg_OriginalPSConsoleHostReadLine -eq $readline) $true 'initialization captures the original readline driver'
            Invoke-ReadBoundary
            $wrapper = $function:PSConsoleHostReadLine
            foreach ($cycle in 1..2) {
                . $IntegrationPath
                Assert-Equal ($Global:__ShellInteg_OriginalPSConsoleHostReadLine -eq $readline) $true 'repeat source preserves the captured readline driver'
                Assert-Equal ($function:PSConsoleHostReadLine -eq $wrapper) $true 'repeat source does not wrap readline again'
                Invoke-ReadBoundary
                Assert-Render (prompt) 'BASE> '
                Assert-Equal $Global:BaseCalls ($cycle + 1) 'one prompt delegation per render after readline initialization'
            }
            Assert-Equal $Global:ReadCalls 3 'one readline delegation per boundary across repeat sources'
        }
        AdoptedLegacyUpgrade {
            Assert-Equal ($Global:__ShellInteg_PrevPrompt -eq $fallback) $true 'upgrade preserves adopted legacy fallback'
            Assert-Equal $Global:ChainCalls 1 'migrated chain called once'
            $delegate = $Global:__ShellInteg_OriginalPrompt
            foreach ($cycle in 1..2) {
                . $IntegrationPath
                Invoke-ReadBoundary
                Assert-Equal ($Global:__ShellInteg_OriginalPrompt -eq $delegate) $true 'repeat source retains adopted delegate'
                Assert-Equal ($Global:__ShellInteg_PrevPrompt -eq $fallback) $true 'repeat source retains legacy fallback'
                Assert-Render (prompt) 'legacy:BASE> '
                Assert-Equal $Global:BaseCalls ($cycle + 1) 'one base call per upgraded render'
                Assert-Equal $Global:ChainCalls ($cycle + 1) 'one chain call per upgraded render'
            }
        }
        StrictLegacyUpgrade {
            Invoke-ReadBoundary
            . $IntegrationPath
            Assert-Render (prompt) 'BASE> '
            Assert-Equal $Global:BaseCalls 2 'strict upgrade retains original delegate'
            function Global:prompt { 'REPLACEMENT> ' }
            Invoke-ReadBoundary
            Assert-Render (prompt) 'REPLACEMENT> '
            Set-Item Function:\global:prompt $legacyWrapper
            Invoke-ReadBoundary
            $beforeBase = $Global:BaseCalls
            Assert-Render (prompt) 'BASE> '
            Assert-Equal $Global:BaseCalls ($beforeBase + 1) 'saved legacy wrapper retains one base call after strict upgrade'
        }
        FirstStatus {
            Add-History -InputObject ([pscustomobject]@{
                CommandLine = 'fixture native failure'; ExecutionStatus = 'Completed'
                StartExecutionTime = [datetime]::Now; EndExecutionTime = [datetime]::Now
            })
            $Global:LASTEXITCODE = 23
            Get-Variable '__Lifecycle_Missing__' -ErrorAction Ignore
            $failed = prompt
            Assert-Render $failed 'BASE> ' 1 23
            Assert-Equal $Global:SeenStatus $false 'native failure reaches original prompt'
            $ok = prompt
            Assert-Render $ok 'BASE> '
            Assert-Equal $Global:SeenStatus $true 'success is not stale native failure'
            Assert-Equal $Global:LASTEXITCODE 23 'native exit status remains untouched'
            Assert-Equal $Global:BaseCalls 3 'one delegate call per prompt'
            Add-History -InputObject ([pscustomobject]@{
                CommandLine = 'fixture PowerShell failure'; ExecutionStatus = 'Completed'
                StartExecutionTime = [datetime]::Now; EndExecutionTime = [datetime]::Now
            })
            $Global:LASTEXITCODE = 0
            Get-Variable '__Lifecycle_Missing__' -ErrorAction Ignore
            $failed = prompt
            Assert-Render $failed 'BASE> ' 1 -1
            Assert-Equal $Global:SeenStatus $false 'PowerShell failure reaches original prompt'
        }
        { $_ -in @('Lifecycle', 'LifecycleMissingFallback') } {
            Assert-Equal ($null -ne $Global:__ShellInteg_Owner) $true 'upgrade creates ownership identity'
            Assert-Equal (Test-Path Function:\__ShellInteg_NewWrapper) $true 'upgrade installs wrapper factory'
            Invoke-ReadBoundary
            function Global:prompt { 'UPGRADED> ' }
            Invoke-ReadBoundary
            Assert-Render (prompt) 'UPGRADED> '
            Set-Item Function:\global:prompt $legacyWrapper
            Invoke-ReadBoundary
            $beforeBase = $Global:BaseCalls
            Assert-Render (prompt) 'BASE> '
            Assert-Equal $Global:BaseCalls ($beforeBase + 1) 'restored legacy wrapper invokes base once'
            $owner = $Global:__ShellInteg_Owner
            . $IntegrationPath
            Assert-Equal ($Global:__ShellInteg_Owner -eq $owner) $true 'repeat source retains upgraded owner'
            foreach ($strictVersion in @('2.0', 'Latest')) {
                function Global:prompt { 'STRICT> ' }
                try {
                    Set-StrictMode -Version $strictVersion
                    Invoke-ReadBoundary
                }
                finally {
                    Set-StrictMode -Off
                }
                Assert-Render (prompt) 'STRICT> '
            }
            Set-Item Function:\global:prompt (__ShellInteg_NewWrapper $Global:__ShellInteg_OriginalPrompt)
            $original = $function:prompt
            $oldActive = $null
            foreach ($cycle in 1..3) {
                foreach ($label in @('alpha', 'beta')) {
                    # Generic environment activation saves and chains the existing
                    # prompt; switching first restores it, like an ordinary deactivate.
                    Set-Item Function:\global:prompt (New-ChainedPrompt $label $original)
                    Invoke-ReadBoundary
                    $beforeBase = $Global:BaseCalls
                    $beforeChain = $Global:ChainCalls
                    $output = prompt
                    Assert-Render $output "${label}:BASE> "
                    Assert-Equal $Global:BaseCalls ($beforeBase + 1) 'saved original invoked once'
                    Assert-Equal $Global:ChainCalls ($beforeChain + 1) 'environment prompt invoked once'
                    if (-not $oldActive) { $oldActive = $function:prompt }
                    Set-Item Function:\global:prompt $original
                    Invoke-ReadBoundary
                    . $IntegrationPath
                    . $IntegrationPath
                    $beforeBase = $Global:BaseCalls
                    $beforeChain = $Global:ChainCalls
                    $output = prompt
                    Assert-Render $output 'BASE> '
                    Assert-Equal $Global:BaseCalls ($beforeBase + 1) 'deactivated original invoked once'
                    Assert-Equal $Global:ChainCalls $beforeChain 'deactivation does not invoke retired environment renderer'
                }
            }
            # A previously adopted wrapper may be restored much later.
            Set-Item Function:\global:prompt $oldActive
            Invoke-ReadBoundary
            $beforeBase = $Global:BaseCalls
            $beforeChain = $Global:ChainCalls
            $output = prompt
            Assert-Render $output 'alpha:BASE> '
            Assert-Equal $Global:BaseCalls ($beforeBase + 1) 'old wrapper retains its original delegate'
            Assert-Equal $Global:ChainCalls ($beforeChain + 1) 'old wrapper does not duplicate its chain'
        }
        Chaining {
            $saved = $function:prompt
            Set-Item Function:\global:prompt (New-ChainedPrompt 'outer' $saved)
            Invoke-ReadBoundary
            $saved = $function:prompt
            Set-Item Function:\global:prompt (New-ChainedPrompt 'inner' $saved)
            Invoke-ReadBoundary
            Add-History -InputObject ([pscustomobject]@{
                CommandLine = 'fixture nested failure'; ExecutionStatus = 'Completed'
                StartExecutionTime = [datetime]::Now; EndExecutionTime = [datetime]::Now
            })
            $Global:LASTEXITCODE = 17
            Get-Variable '__Lifecycle_Missing__' -ErrorAction Ignore
            $output = prompt
            Assert-Render $output 'inner:outer:BASE> ' 1 17
            Assert-Equal $Global:SeenStatus $false 'failure reaches deepest saved delegate'
            Assert-Equal $Global:BaseCalls 2 'deepest delegate called once'
            Assert-Equal $Global:ChainCalls 2 'each nested renderer called once'
            Assert-Equal $Global:LASTEXITCODE 17 'nested render preserves native status'
            $output = prompt
            Assert-Render $output 'inner:outer:BASE> '
            Assert-Equal $Global:SeenStatus $true 'nested success is not stale failure'
        }
        WorkChangingChain {
            $saved = $function:prompt
            Set-Item Function:\global:prompt (New-WorkChangingPrompt $saved)
            Invoke-ReadBoundary
            Add-History -InputObject ([pscustomobject]@{
                CommandLine = 'fixture work-changing failure'; ExecutionStatus = 'Completed'
                StartExecutionTime = [datetime]::Now; EndExecutionTime = [datetime]::Now
            })
            $beforeBase = $Global:BaseCalls
            $Global:LASTEXITCODE = 17
            Get-Variable '__Lifecycle_Missing__' -ErrorAction Ignore
            $output = prompt
            $seen = $Global:SeenStatus
            Assert-Render $output $control.Text 1 17
            Assert-Equal $seen $control.Status 'deepest status matches no-integration control'
            Assert-Equal ($Global:BaseCalls - $beforeBase) $control.BaseCalls 'base call count matches control'
            Assert-Equal $Global:ChainCalls $control.ChainCalls 'chain call count matches control'
            Assert-Equal $Global:LASTEXITCODE $control.ExitCode 'native status matches control'
        }
        Throwing {
            function Global:prompt { $Global:ChainCalls++; throw 'fixture renderer failure' }
            Invoke-ReadBoundary
            $output = prompt
            Assert-Render $output ''
            Assert-Equal $Global:ChainCalls 1 'throwing renderer invoked only once'
            function Global:prompt { $Global:ChainCalls++; return $null }
            Invoke-ReadBoundary
            $output = prompt
            Assert-Render $output ''
            Assert-Equal $Global:ChainCalls 2 'empty renderer invoked once without a fabricated prompt'
            function Global:prompt { 'RECOVERED> ' }
            Invoke-ReadBoundary
            $output = prompt
            Assert-Render $output 'RECOVERED> '
        }
        FailSafe {
            $saved = $function:prompt
            Set-Item Function:\global:prompt (New-ChainedPrompt 'fallback' $saved)
            Invoke-ReadBoundary
            function Global:Get-History { throw 'fixture integration infrastructure failure' }
            try {
                $output = prompt
                Assert-Equal $output 'fallback:BASE> ' 'integration fail-safe preserves chained visible prompt'
                Assert-Equal $Global:BaseCalls 2 'fail-safe invokes deepest delegate once'
                Assert-Equal $Global:ChainCalls 1 'fail-safe invokes adopted renderer once'
            }
            finally {
                Remove-Item Function:\Get-History
            }
            $output = prompt
            Assert-Render $output 'fallback:BASE> '
            Assert-Equal $Global:BaseCalls 3 'subsequent prompt recovers from infrastructure failure'
            Assert-Equal $Global:ChainCalls 2 'subsequent renderer is not suppressed or repeated'
        }
        Constrained {
            Assert-Equal "$($ExecutionContext.SessionState.LanguageMode)" 'ConstrainedLanguage' 'child remains constrained'
            Assert-Equal $Global:__ShellInteg_CanInspectErrors $false 'CLM baseline avoids parser and closure path'
            Invoke-ReadBoundary
            Assert-Equal $Global:__ShellInteg_LastSubmittedLine $null 'CLM does not install automatic readline driver'
            function Global:prompt { 'LATE> ' }
            $output = prompt
            Assert-Equal $output 'LATE> ' 'CLM takeover is not automatically healed'
            . $IntegrationPath
            $output = prompt
            Assert-Render $output 'LATE> '
            . $IntegrationPath
            $output = prompt
            Assert-Render $output 'LATE> '
            Assert-Equal "$($ExecutionContext.SessionState.LanguageMode)" 'ConstrainedLanguage' 'manual source does not relax CLM'
        }
    }
    @{ Scenario = $Scenario; Version = "$($PSVersionTable.PSVersion)"; Assertions = $Global:Assertions } |
        ConvertTo-Json -Compress
    exit 0
}
catch {
    [Console]::Error.WriteLine($_.ToString())
    exit 1
}
