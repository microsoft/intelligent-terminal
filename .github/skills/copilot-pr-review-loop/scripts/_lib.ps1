# Shared helpers for copilot-pr-review-loop scripts.
# Dot-source with: `. "$PSScriptRoot/_lib.ps1"`
#
# Dot-sourcing runs the prerequisite check below; if `gh` is missing or
# unauthenticated the script halts BEFORE doing any work, with a single
# actionable error message the calling agent can pattern-match on.
#
# Compatibility: Windows PowerShell 5.1+ and PowerShell 7+. Uses only
# `& gh @args 2>$tempFile` for stdout/stderr separation — avoids
# `System.Diagnostics.ProcessStartInfo.ArgumentList` which is .NET
# Core / .NET 5+ only and returns $null on .NET Framework.

# Prerequisite check: gh CLI installed AND authenticated.
# Fails fast with install/login instructions. Idempotent (once per
# PowerShell session).
function Assert-GhReady {
    if ($script:_GhReady) { return }

    # 1. Installed?
    $cmd = Get-Command gh -ErrorAction SilentlyContinue
    if (-not $cmd) {
        throw @'
copilot-pr-review-loop: prerequisite missing — `gh` CLI is not on PATH.

Install (one of):
  - winget install --id GitHub.cli           (Windows)
  - brew install gh                          (macOS)
  - sudo apt install gh                      (Debian/Ubuntu — see https://cli.github.com for other distros)
  - https://cli.github.com/                  (universal installer + download)

Then `gh auth login` and re-run this command.
'@
    }

    # 2. Authenticated? `gh auth status` exits non-zero when no account
    # is logged in. Capture stderr to a temp file via the `2>` redirect.
    $errFile = [IO.Path]::GetTempFileName()
    try {
        $null = & gh auth status 2>$errFile
        $ec = $LASTEXITCODE
        if ($ec -ne 0) {
            $err = ''
            if (Test-Path -LiteralPath $errFile) {
                $err = (Get-Content -Raw -LiteralPath $errFile -ErrorAction SilentlyContinue)
                if ($null -eq $err) { $err = '' }
            }
            throw @"
copilot-pr-review-loop: prerequisite missing — ``gh`` CLI is not authenticated.

Run:
  gh auth login

Then re-run this command. (``gh auth status`` reported:
  $($err.Trim()))
"@
        }
    } finally {
        if (Test-Path -LiteralPath $errFile) {
            Remove-Item -LiteralPath $errFile -ErrorAction SilentlyContinue
        }
    }

    $script:_GhReady = $true
}

# Single-invocation gh wrapper. Captures stdout + stderr separately
# via the `2>` redirect to a temp file. Returns ExitCode/Stdout/Stderr
# so callers never have to re-invoke `gh` just to recover stderr, and
# never feed stderr into `ConvertFrom-Json` on success.
#
# Note on -WhatIf: PowerShell's `2>` redirect goes through Out-File,
# which respects $WhatIfPreference at the caller scope. The bundled
# `10-cleanup-outdated.ps1` therefore uses an explicit `-DryRun`
# switch instead of [CmdletBinding(SupportsShouldProcess)], so this
# helper never sees a leaked WhatIfPreference and never prints
# "Performing the operation Output to File" noise.
function Invoke-Gh {
    param([Parameter(Mandatory)][string[]]$GhArgs)
    $errFile = [IO.Path]::GetTempFileName()
    try {
        $out = & gh @GhArgs 2>$errFile
        $ec = $LASTEXITCODE
        $err = ''
        if (Test-Path -LiteralPath $errFile) {
            $err = (Get-Content -Raw -LiteralPath $errFile -ErrorAction SilentlyContinue)
            if ($null -eq $err) { $err = '' }
        }
        [pscustomobject]@{ ExitCode = $ec; Stdout = ($out | Out-String); Stderr = $err }
    } finally {
        if (Test-Path -LiteralPath $errFile) {
            Remove-Item -LiteralPath $errFile -ErrorAction SilentlyContinue
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

# Run the prerequisite check as a side-effect of dot-sourcing.
Assert-GhReady
