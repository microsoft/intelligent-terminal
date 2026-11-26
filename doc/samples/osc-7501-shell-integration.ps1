# Shell Integration — non-invasive prompt wrapper
# Emits OSC 133 (command marks / exit code) and OSC 9;9 (CWD) escape
# sequences WITHOUT altering the visual appearance of the user's prompt.
#
# Compatible with Windows PowerShell 5.1+ and PowerShell 7+.
# Safe to source multiple times (idempotent guard).

if ($env:INTELLIGENT_TERMINAL -ne '1') {
    return
}

if (-not $Global:__ShellInteg_Installed) {

    # ── Escape characters (PS 5.1 doesn't support `e / `a literals) ──
    $Global:__ShellInteg_ESC = [char]0x1B   # ESC
    $Global:__ShellInteg_BEL = [char]0x07   # BEL (OSC string terminator)

    # ── Snapshot the user's current prompt before we touch it ──────────
    $Global:__ShellInteg_OriginalPrompt = $function:prompt
    $Global:__ShellInteg_LastHistoryId  = -1
    $Global:__ShellInteg_LastErrorRecord = $Error[0]
    $Global:__ShellInteg_LastSubmittedLine = $null
    $Global:__ShellInteg_StatusPending = $false
    $Global:__ShellInteg_Wrappers = $null
    $Global:__ShellInteg_PrevPrompt = $null
    $Global:__ShellInteg_Rendering = $false
    $Global:__ShellInteg_CanInspectErrors =
        $ExecutionContext.SessionState.LanguageMode -eq 'FullLanguage'
    $Global:__ShellInteg_Installed      = $true

    # PowerShell 7 drops parser failures before prompt runs, leaving no
    # observable status there. Retain the submitted line at the PSReadLine
    # boundary without changing what is returned to the engine.
    #
    # This boundary is also where we re-arm (see __ShellInteg_Rearm): it runs
    # once per command, BEFORE the user's command executes, so it cannot
    # disturb the $? that the next prompt reads.
    if ($Global:__ShellInteg_CanInspectErrors -and (Test-Path Function:\PSConsoleHostReadLine)) {
        function Global:__ShellInteg_GetResumeTarget {
            param([string]$line)
            $tokens = $null
            $parseErrors = $null
            $ast = [System.Management.Automation.Language.Parser]::ParseInput(
                $line, [ref]$tokens, [ref]$parseErrors)
            if ($parseErrors.Count -ne 0 -or $null -eq $ast.EndBlock -or
                $ast.BeginBlock -or $ast.ProcessBlock -or $ast.ParamBlock -or
                $ast.EndBlock.Statements.Count -ne 1) { return }
            $pipeline = $ast.EndBlock.Statements[0]
            if ($pipeline -isnot [System.Management.Automation.Language.PipelineAst] -or
                $pipeline.PipelineElements.Count -ne 1) { return }
            if ($pipeline.PSObject.Properties['Background'] -and $pipeline.Background) { return }
            $command = $pipeline.PipelineElements[0]
            if ($command -isnot [System.Management.Automation.Language.CommandAst] -or
                $command.InvocationOperator -eq [System.Management.Automation.Language.TokenKind]::Dot) { return }
            if ($command.GetCommandName() -notmatch '^(?:.*[\\/])?(copilot|claude|gemini|codex|opencode|agy)(?:\.(?:exe|cmd|bat))?$') { return }
            $resumeAgent = $Matches[1].ToLowerInvariant()
            if ($resumeAgent -eq 'agy') { $resumeAgent = 'antigravity' }
            $arguments = @()
            for ($i = 1; $i -lt $command.CommandElements.Count; $i++) {
                $element = $command.CommandElements[$i]
                if ($element -isnot [System.Management.Automation.Language.StringConstantExpressionAst]) { return }
                $arguments += $element
            }
            $resumeSession = $null
            if ($resumeAgent -eq 'codex') {
                if ($arguments.Count -lt 2 -or
                    $arguments[0].StringConstantType -ne [System.Management.Automation.Language.StringConstantType]::BareWord -or
                    $arguments[0].Value -cne 'resume') { return }
                $resumeSession = $arguments[1].Value
            }
            else {
                $flag = if ($resumeAgent -eq 'opencode') { '--session' }
                        elseif ($resumeAgent -eq 'antigravity') { '--conversation' }
                        else { '--resume' }
                for ($i = 0; $i -lt $arguments.Count; $i++) {
                    # Quoted IDs are allowed, but a wholly quoted argument is
                    # not a resume flag. Preserve the AST's quote boundary.
                    if ($arguments[$i].StringConstantType -ne [System.Management.Automation.Language.StringConstantType]::BareWord) { continue }
                    $argument = $arguments[$i].Value
                    if ($argument -ceq '--') { break }
                    if ($argument -ceq $flag) {
                        if ($null -ne $resumeSession -or $i + 1 -ge $arguments.Count) { return }
                        $resumeSession = $arguments[++$i].Value
                    }
                    elseif ($argument.StartsWith($flag + '=', [StringComparison]::Ordinal)) {
                        if ($null -ne $resumeSession) { return }
                        $resumeSession = $argument.Substring($flag.Length + 1)
                    }
                }
            }
            if ($null -eq $resumeSession -or $resumeSession.Length -gt 256 -or
                $resumeSession.StartsWith('-', [StringComparison]::Ordinal) -or
                $resumeSession -notmatch '\A[A-Za-z0-9_.:-]+\z' -or
                $resumeSession.StartsWith('sidekick-', [StringComparison]::Ordinal)) { return }
            return @{ Agent = $resumeAgent; SessionId = $resumeSession }
        }

        $Global:__ShellInteg_OriginalPSConsoleHostReadLine = $function:PSConsoleHostReadLine
        function Global:PSConsoleHostReadLine {
            __ShellInteg_Rearm
            $line = & $Global:__ShellInteg_OriginalPSConsoleHostReadLine @args
            $Global:__ShellInteg_LastSubmittedLine =
                if ($line -is [string]) { $line } else { $null }
            if ($line -is [string]) {
                $resume = __ShellInteg_GetResumeTarget $line
                if ($null -ne $resume) {
                    $resumeMark = '{0}]9001;Resume;{1};{2}{3}' -f
                        $Global:__ShellInteg_ESC, $resume.Agent, $resume.SessionId,
                        $Global:__ShellInteg_BEL
                    [Console]::Write($resumeMark)
                }
            }
            if (-not [string]::IsNullOrWhiteSpace($line)) {
                $Global:__ShellInteg_StatusPending = $true
                [Console]::Write("$($Global:__ShellInteg_ESC)]7501;state=working$($Global:__ShellInteg_BEL)")
            }
            return $line
        }
    }

    function Global:__ShellInteg_GetLastExitCode {
        # $? still reflects the *user's* last command here because this
        # is the very first call inside the prompt function.
        if ($? -eq $True) { return 0 }
        # $? is False -> the last command failed. Preserve a real non-zero
        # native exit code. PowerShell-level errors leave the previous value
        # untouched, while command-not-found can leave it null because no
        # native process started; zero/null therefore use a numeric sentinel.
        if ($null -ne $LastExitCode -and $LastExitCode -ne 0) {
            return $LastExitCode
        }
        return -1
    }

    function Global:__ShellInteg_IsDriveSwitchFailure {
        param($ast, $errorRecord)
        if ($ErrorActionPreference -ne 'Continue' -or
            $errorRecord.FullyQualifiedErrorId -cne 'DriveNotFound,Microsoft.PowerShell.Commands.SetLocationCommand' -or
            $errorRecord.InvocationInfo.MyCommand -isnot [System.Management.Automation.CmdletInfo] -or
            $errorRecord.InvocationInfo.MyCommand.ImplementingType.FullName -cne 'Microsoft.PowerShell.Commands.SetLocationCommand') {
            return $false
        }
        # ErrorRecords do not retain whether an ErrorAction default suppressed
        # their display. Leave these configurations to the normal $? path.
        foreach ($key in $PSDefaultParameterValues.Keys) {
            if ($key -notlike '*:*') { continue }
            $parameterPattern = ($key -split ':', 2)[1]
            if ($parameterPattern -and
                ('ErrorAction' -like $parameterPattern -or 'ea' -like $parameterPattern)) { return $false }
        }
        if ($null -eq $ast.EndBlock -or $ast.BeginBlock -or
            $ast.ProcessBlock -or $ast.ParamBlock -or
            $ast.UsingStatements.Count -ne 0 -or
            $ast.EndBlock.Statements.Count -ne 1) { return $false }
        $pipeline = $ast.EndBlock.Statements[0]
        if ($pipeline -isnot [System.Management.Automation.Language.PipelineAst] -or
            $pipeline.PipelineElements.Count -ne 1) { return $false }
        if ($pipeline.PSObject.Properties['Background'] -and $pipeline.Background) { return $false }
        $command = $pipeline.PipelineElements[0]
        if ($command -isnot [System.Management.Automation.Language.CommandAst] -or
            $command.InvocationOperator -ne [System.Management.Automation.Language.TokenKind]::Unknown -or
            $command.CommandElements.Count -ne 1 -or
            $command.Redirections.Count -ne 0) { return $false }
        $name = $command.GetCommandName()
        if ($name -notmatch '\A[A-Za-z]:\z' -or
            $errorRecord.TargetObject -ine $name.Substring(0, 1)) { return $false }
        $function = Get-Command -Name $name -ErrorAction Ignore
        $definition = 'Set-Location $MyInvocation.MyCommand.Name'
        return $function -is [System.Management.Automation.FunctionInfo] -and
            $function.Definition.Trim() -ceq $definition -and
            $errorRecord.InvocationInfo.Line.Trim() -ceq $definition
    }

    function prompt {
        # ── Capture exit code FIRST — before anything else can clobber $? ──
        $gle           = $(__ShellInteg_GetLastExitCode)

        # Only the outermost wrapper emits command marks.
        if ($Global:__ShellInteg_Rendering) {
            if (-not $Global:__ShellInteg_CanInspectErrors) {
                if ($Global:__ShellInteg_PrevPrompt) {
                    return (& $Global:__ShellInteg_PrevPrompt)
                }
                return ''
            }
            if ($gle -eq 0) { $null = $true }
            else { Get-Variable '__ShellInteg_NoSuchVariable__' -ErrorAction Ignore }
            return (& $__ShellInteg_OriginalPrompt)
        }

        try {
            $submittedLine = $Global:__ShellInteg_LastSubmittedLine
            # GetNewClosure creates a module with its own error list.
            $errorRecord   = $Global:Error[0]
            $entry         = Get-History -Count 1
            $loc           = $executionContext.SessionState.Path.CurrentLocation
            $E             = $Global:__ShellInteg_ESC
            $B             = $Global:__ShellInteg_BEL

            $prefix = ''
            $suffix = ''

            # ── Previous command finished (OSC 133;D with exit code) ──
            $historyAdvanced = $entry -and $entry.Id -ne $Global:__ShellInteg_LastHistoryId
            $newErrorRecord = $Global:__ShellInteg_CanInspectErrors -and
                $null -ne $errorRecord -and
                -not [object]::ReferenceEquals($errorRecord, $Global:__ShellInteg_LastErrorRecord)
            $inputHadParserError = $false
            $inputHadDriveSwitchError = $false
            if ($Global:__ShellInteg_CanInspectErrors -and
                $gle -eq 0 -and
                $null -ne $submittedLine -and
                ($newErrorRecord -or -not $historyAdvanced)) {
                $tokens = $null
                $parseErrors = $null
                $inputAst = [System.Management.Automation.Language.Parser]::ParseInput(
                    $submittedLine,
                    [ref]$tokens,
                    [ref]$parseErrors)
                $inputHadParserError = $parseErrors.Count -gt 0
                if (-not $inputHadParserError -and $newErrorRecord) {
                    $inputHadDriveSwitchError = __ShellInteg_IsDriveSwitchFailure $inputAst $errorRecord
                }
            }
            if ($inputHadParserError -and $gle -eq 0) {
                $gle = -1
            }
            if ($inputHadDriveSwitchError -and $gle -eq 0) {
                $gle = -1
            }
            $newUntrackedError = -not $historyAdvanced -and $gle -ne 0 -and $newErrorRecord
            if ($historyAdvanced -or $newUntrackedError -or $inputHadParserError) {
                $prefix += "${E}]133;D;${gle}${B}"
            }

            # ── Prompt started (OSC 133;A) ──
            if ($Global:__ShellInteg_StatusPending) {
                $state = if ($gle -eq 0) { 'done' } else { 'error' }
                $prefix += "${E}]7501;state=${state}${B}"
                $Global:__ShellInteg_StatusPending = $false
            }
            $prefix += "${E}]133;A${B}"

            # ── Report current working directory (OSC 9;9) ──
            $prefix += "${E}]9;9;`"${loc}`"${B}"

            # ── Report shell identity (OSC 9001;ShellType) ──
            # Emitted every prompt so the terminal always knows which shell owns
            # the pane, even after a nested shell (e.g. wsl) exits and PowerShell
            # repaints its prompt. PSEdition 'Core' is pwsh 7+, 'Desktop' is
            # Windows PowerShell 5.1.
            $shellName = if ($PSVersionTable.PSEdition -eq 'Core') { 'pwsh' } else { 'powershell' }
            $prefix += "${E}]9001;ShellType;${shellName};$($PSVersionTable.PSVersion)${B}"

            # ── Prompt ended, command input starts (OSC 133;B) ──
            $suffix = "${E}]133;B${B}"

            # ── Restore a success/failure signal for the prompt we wrap ──
            # Our work above has overwritten $?, so a prompt that reads it (Oh My
            # Posh's status segment, starship, posh-git) would wrongly see success.
            # $? cannot be assigned, but it mirrors the last statement: looking up
            # a variable that does not exist fails WITHOUT touching $Error, and a
            # trivial assignment succeeds. This must be the statement immediately
            # before the delegation.
            #
            # The value restored is derived from $gle, not the literal $? the
            # engine had: parser and drive-switch checks can turn a $?-true
            # command into $gle -1. That is deliberate - $gle is the same failure signal
            # reported in OSC 133;D, so the wrapped prompt and the terminal agree
            # on whether the command failed.
            $Global:__ShellInteg_Rendering = $true
            try {
            if ($gle -eq 0) { $null = $true }
            else { Get-Variable '__ShellInteg_NoSuchVariable__' -ErrorAction Ignore }
            # ── Delegate to the user's ORIGINAL prompt — visual output is theirs ──
            # Failures are caught HERE, at the delegation, so the wrapped prompt
            # is invoked exactly once per render. Letting it escape to the outer
            # catch would run it a second time and duplicate its side effects.
            # An empty result is correct: the host then supplies its own prompt.
            try     { $originalOutput = & $__ShellInteg_OriginalPrompt }
            catch   { $originalOutput = '' }
            }
            finally { $Global:__ShellInteg_Rendering = $false }

            $Global:__ShellInteg_LastHistoryId = if ($entry) { $entry.Id } else { -1 }
            $Global:__ShellInteg_LastErrorRecord = $Global:Error[0]
            $Global:__ShellInteg_LastSubmittedLine = $null

            return "${prefix}${originalOutput}${suffix}"
            }
        catch {
            # FAIL SAFE for failures in OUR OWN code above (Get-History, the
            # parser probe, string building). The wrapped prompt has its own
            # catch at the delegation, so reaching here means it has NOT yet
            # been invoked for this render - delegating now keeps it at exactly
            # one invocation.
            #
            # Nested wrappers must not emit marks during fallback rendering.
            $Global:__ShellInteg_Rendering = $true
            try { return (& $__ShellInteg_OriginalPrompt) }
            catch {
                # The wrapped prompt itself threw. Return an empty string and
                # let the HOST fall back to its own built-in prompt. Do not
                # fabricate one: this wrapper never alters the visible prompt,
                # and a hand-rolled "PS <cwd>> " is only an imitation of the
                # real default, which differs by host and edition.
                return ''
            }
            finally { $Global:__ShellInteg_Rendering = $false }
        }
    }

    $Global:__ShellInteg_Wrapper = $function:prompt
    if ($Global:__ShellInteg_CanInspectErrors) {
        $Global:__ShellInteg_PromptTemplate = $function:prompt
        $Global:__ShellInteg_Wrappers = New-Object 'System.Runtime.CompilerServices.ConditionalWeakTable[System.Management.Automation.ScriptBlock,System.Object]'
        function Global:__ShellInteg_NewWrapper {
            param([scriptblock]$__ShellInteg_OriginalPrompt)
            # Capture this parameter for the template's unqualified delegate lookup.
            return $Global:__ShellInteg_PromptTemplate.GetNewClosure()
        }
        $Global:__ShellInteg_Wrapper = __ShellInteg_NewWrapper $Global:__ShellInteg_OriginalPrompt
        $Global:__ShellInteg_Wrappers.Add($Global:__ShellInteg_Wrapper, $true)
        Set-Item Function:\global:prompt $Global:__ShellInteg_Wrapper
    }
}

# Recognize restored wrappers as well as the most recent one. Weak keys avoid
# retaining every transient renderer for the lifetime of an interactive shell.
function Global:__ShellInteg_Rearm {
    # An absent registry variable identifies a pre-closure session; do not migrate it.
    if (-not (Get-Variable -Name __ShellInteg_Wrappers -Scope Global -ErrorAction Ignore)) { return }
    if ($null -eq $Global:__ShellInteg_Wrapper) { return }
    $current = $function:prompt
    if ($null -eq $current) { return }
    if ($current -eq $Global:__ShellInteg_Wrapper) { return }

    # Best-effort. This runs from PSConsoleHostReadLine, so a throw here would
    # break the input path, and at script load, where it would break profile
    # sourcing. `prompt` can legitimately be ReadOnly or Constant in a hardened
    # profile, which makes Set-Item raise SessionStateUnauthorizedAccessException.
    # Shell integration is a convenience; never let it take the shell down.
    try {
        # ConstrainedLanguage cannot use closures; retain the shared-delegate path.
        if (-not $Global:__ShellInteg_CanInspectErrors) {
            if ($current -ne $Global:__ShellInteg_OriginalPrompt) {
                $Global:__ShellInteg_PrevPrompt = $Global:__ShellInteg_OriginalPrompt
                $Global:__ShellInteg_OriginalPrompt = $current
            }
            Set-Item Function:\global:prompt $Global:__ShellInteg_Wrapper -ErrorAction Stop
            return
        }
        $known = $null
        if ($Global:__ShellInteg_Wrappers.TryGetValue($current, [ref]$known)) {
            $Global:__ShellInteg_Wrapper = $current
            return
        }
        $wrapper = __ShellInteg_NewWrapper $current
        $Global:__ShellInteg_Wrappers.Add($wrapper, $true)
        Set-Item Function:\global:prompt $wrapper -ErrorAction Stop
        $Global:__ShellInteg_Wrapper = $wrapper
    }
    catch {
        # Leave whatever prompt is installed alone; try again next command.
    }
}

__ShellInteg_Rearm
