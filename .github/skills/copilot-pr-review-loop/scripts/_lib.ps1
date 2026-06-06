# Shared helpers for copilot-pr-review-loop scripts.
# Dot-source with: `. "$PSScriptRoot/_lib.ps1"`

# Single-invocation gh wrapper. Captures stdout + stderr separately (via
# temp file) and returns ExitCode/Stdout/Stderr so callers never have to
# re-invoke `gh` just to recover stderr, and never feed stderr into
# `ConvertFrom-Json` on success.
function Invoke-Gh {
    param([Parameter(Mandatory)][string[]]$GhArgs)
    # Bypass any caller WhatIfPreference / ConfirmPreference inheritance — these
    # are coordination preferences for the script's own mutating ops, not for
    # the internal gh capture pipeline. Without this, `2>$tempFile` prints
    # "What if: Performing the operation Output to File" noise on -WhatIf
    # runs and the temp file is never written, so $err is empty.
    $WhatIfPreference = $false
    $ConfirmPreference = 'None'
    $errFile = [IO.Path]::GetTempFileName()
    try {
        $out = & gh @GhArgs 2>$errFile
        $ec = $LASTEXITCODE
        $err = ''
        if ([System.IO.File]::Exists($errFile)) {
            $err = [System.IO.File]::ReadAllText($errFile)
        }
        [pscustomobject]@{ ExitCode = $ec; Stdout = ($out | Out-String); Stderr = $err }
    } finally {
        if ([System.IO.File]::Exists($errFile)) {
            [System.IO.File]::Delete($errFile)
        }
    }
}

# Wrapper around Invoke-Gh for `gh api graphql` that throws on either
# non-zero exit OR a GraphQL `errors` array in the response body.
function Invoke-GhGraphQL {
    param(
        [Parameter(Mandatory)][string[]]$GhArgs,
        [Parameter(Mandatory)][string]$Context
    )
    $r = Invoke-Gh -GhArgs (@('api','graphql') + $GhArgs)
    if ($r.ExitCode -ne 0) {
        throw "gh api graphql failed (exit $($r.ExitCode)) [$Context]: $($r.Stderr)"
    }
    $data = $r.Stdout | ConvertFrom-Json
    if ($data.errors) {
        $msgs = ($data.errors | ForEach-Object { $_.message }) -join '; '
        throw "GraphQL errors [$Context]: $msgs"
    }
    $data
}

# Auto-resolve owner/repo from gh's local context when caller didn't pass them.
function Resolve-RepoCoords {
    param([string]$Owner, [string]$Repo)
    if ($Owner -and $Repo) { return @{ Owner = $Owner; Repo = $Repo } }
    $r = Invoke-Gh -GhArgs @('repo','view','--json','owner,name')
    if ($r.ExitCode -ne 0) {
        throw "gh repo view failed (exit $($r.ExitCode)): $($r.Stderr). Pass -Owner and -Repo explicitly, or run from inside a gh-detected repo."
    }
    $info = $r.Stdout | ConvertFrom-Json
    @{
        Owner = if ($Owner) { $Owner } else { $info.owner.login }
        Repo  = if ($Repo)  { $Repo }  else { $info.name }
    }
}
