<#
.SYNOPSIS
    Regenerate the Rust-crate third-party attribution artifacts for tools/wta.

.DESCRIPTION
    Runs `cargo metadata --filter-platform x86_64-pc-windows-msvc` against
    tools/wta/Cargo.toml, walks the resolved graph keeping only normal
    (non-dev, non-build) dependencies, and in a single pass produces:

      1. tools/wta/cgmanifest.json -- Microsoft Component Governance
         manifest listing every statically-linked Rust crate with Cargo
         provenance. This is the formal compliance artifact that CG
         tooling auto-discovers; it sits alongside the existing
         oss/<name>/cgmanifest.json files.

      2. The marker-guarded `<!-- BEGIN wta-rust-deps -->` block in
         /NOTICE.md -- compact human-readable attribution (one bullet
         per (name, version) plus one canonical license-text section
         per unique atomic license identifier at the bottom). License
         expressions are normalized: `X/Y` is rewritten to `X OR Y`,
         whitespace collapsed, and tokens in pure-OR expressions sorted
         so equivalent expressions in different orderings collapse to a
         single canonical form. Composite expressions like
         `(MIT OR Apache-2.0) AND Unicode-3.0` are decomposed into their
         atomic tokens so every required license text is present.

    Target-cfg filtering is delegated to cargo itself via --filter-platform,
    so the script does not maintain its own list of non-Windows tokens.
    License text is sourced from the local Cargo registry cache (the
    extracted src/ tree, then the gzipped .crate tarball), with a
    best-effort fall back to the upstream GitHub raw LICENSE file.

    The /NOTICE.md edit is performed atomically and preserves the file's
    original CRLF line endings; every section outside the marker block is
    left byte-identical.

.NOTES
    Requires `cargo` on PATH. The repo's rust-toolchain.toml pins a
    custom channel for CI builds; for metadata-only invocations bypass
    it by setting `$env:RUSTUP_TOOLCHAIN = 'stable'` before invoking
    the script.

.EXAMPLE
    PS> $env:RUSTUP_TOOLCHAIN = 'stable'
    PS> .\build\scripts\Generate-WtaThirdPartyNotices.ps1
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

# Requires PowerShell 7+. This matches the existing
# build/scripts/Generate-ThirdPartyNotices.ps1 convention, which uses
# ConvertFrom-Markdown from PS 6.1+. ConvertFrom-Json / ConvertTo-Json
# also produce different output across versions (5.1 over-indents and lacks
# `-Depth` on ConvertFrom-Json), so requiring 7 keeps generated files
# byte-deterministic.
if ($PSVersionTable.PSVersion.Major -lt 7) {
    throw "This script requires PowerShell 7 or later. Current version: $($PSVersionTable.PSVersion). Run with pwsh.exe."
}

$repoRoot       = Resolve-Path (Join-Path $PSScriptRoot '..\..')
$wtaRoot        = Join-Path $repoRoot 'tools\wta'
$noticePath     = Join-Path $repoRoot 'NOTICE.md'
$cgManifestPath = Join-Path $wtaRoot 'cgmanifest.json'
$targetTriple   = 'x86_64-pc-windows-msvc'

# ---------------------------------------------------------------------------
# 1. cargo metadata (parsed in memory; --filter-platform delegates the
#    target-cfg filtering to cargo so no hand-rolled OS allow/deny list is
#    needed in this script).
# ---------------------------------------------------------------------------
Write-Host "Running cargo metadata for $wtaRoot\Cargo.toml (--filter-platform $targetTriple)" -ForegroundColor Cyan
$cargoJson = & cargo metadata --format-version 1 `
    --filter-platform $targetTriple `
    --manifest-path (Join-Path $wtaRoot 'Cargo.toml')
if ($LASTEXITCODE -ne 0) {
    throw "cargo metadata failed with exit code $LASTEXITCODE. Is cargo on PATH? Try `$env:RUSTUP_TOOLCHAIN='stable'."
}
$meta = $cargoJson | ConvertFrom-Json -Depth 100

$packages = @{}
foreach ($p in $meta.packages) { $packages[$p.id] = $p }
$nodes = @{}
foreach ($n in $meta.resolve.nodes) { $nodes[$n.id] = $n }
$rootId = $meta.resolve.root

# ---------------------------------------------------------------------------
# 2. Walk the dep graph (DFS via Stack) keeping only normal (non-dev,
#    non-build) edges. Traversal order doesn't matter -- the resulting
#    `$seen` set is fed unsorted into the alphabetical attribution pass below.
#    Target-cfg filtering already happened in cargo via --filter-platform.
# ---------------------------------------------------------------------------
function Test-NormalDep {
    param($Dep)
    $kinds = $Dep.dep_kinds
    if (-not $kinds) { return $true }
    foreach ($k in $kinds) {
        if ($k.kind -ne 'dev' -and $k.kind -ne 'build') { return $true }
    }
    return $false
}

$seen = New-Object 'System.Collections.Generic.HashSet[string]'
$stack = New-Object 'System.Collections.Generic.Stack[string]'
[void]$stack.Push($rootId)
while ($stack.Count -gt 0) {
    $nid = $stack.Pop()
    if (-not $seen.Add($nid)) { continue }
    $node = $nodes[$nid]
    if (-not $node) { continue }
    foreach ($dep in $node.deps) {
        if (Test-NormalDep $dep) { [void]$stack.Push($dep.pkg) }
    }
}
[void]$seen.Remove($rootId)

# ---------------------------------------------------------------------------
# 3. Enumerate every reachable (name, version) package directly. Do NOT
#    dedup by name -- when cargo resolves multiple versions of one crate
#    (e.g. syn 1 + syn 2 both linked into the binary) all of them require
#    attribution.
# ---------------------------------------------------------------------------
$attributed = @()
foreach ($pkgId in $seen) {
    $pkg = $packages[$pkgId]
    if (-not $pkg -or -not $pkg.source) { continue }   # skip path-only deps
    $attributed += $pkg
}
$attributed = $attributed | Sort-Object name, version
Write-Host "Crates to attribute: $($attributed.Count)" -ForegroundColor Cyan

# ---------------------------------------------------------------------------
# 4. SPDX normalization + atomic-token decomposition.
#    -  `X/Y` -> `X OR Y` (legacy cargo notation)
#    -  collapse whitespace
#    -  in pure-OR expressions, sort tokens so `MIT OR Apache-2.0` and
#       `Apache-2.0 OR MIT` collapse to the same canonical string.
#    -  atomic decomposition strips parens and splits on ` OR ` and ` AND `
#       so a single license-text section is emitted per unique atomic
#       identifier (covers composites like `(MIT OR Apache-2.0) AND Unicode-3.0`).
# ---------------------------------------------------------------------------
function Get-NormalizedSpdx {
    param([string]$Raw)
    if (-not $Raw) { return '(unspecified)' }
    $s = $Raw -replace '\s*/\s*', ' OR '   # MIT/Apache-2.0
    $s = ($s -replace '\s+', ' ').Trim()
    if ($s -notmatch '\(|\)|\bAND\b') {
        $tokens = ($s -split '\s+OR\s+') | Sort-Object -Unique
        $s = $tokens -join ' OR '
    }
    return $s
}

function Get-SpdxAtoms {
    param([string]$Normalized)
    if (-not $Normalized -or $Normalized -eq '(unspecified)') { return @($Normalized) }
    # Strip parens, split on OR / AND / WITH, trim, drop empties.
    $stripped = $Normalized -replace '[()]', ' '
    $atoms = $stripped -split '\s+(?:OR|AND|WITH)\s+' | ForEach-Object { $_.Trim() } | Where-Object { $_ }
    return ($atoms | Sort-Object -Unique)
}

function Get-SpdxCanonicalText {
    param([string]$Atom)
    if (-not $Atom -or $Atom -eq '(unspecified)') { return $null }
    # SPDX project publishes the canonical text for every listed identifier at
    # https://raw.githubusercontent.com/spdx/license-list-data/main/text/<id>.txt
    # — this is the authoritative source and the right fallback when we cannot
    # confidently pick a matching file from the crate's own tree.
    $url = "https://raw.githubusercontent.com/spdx/license-list-data/main/text/$Atom.txt"
    try {
        $resp = Invoke-WebRequest -Uri $url -UseBasicParsing -TimeoutSec 15 `
            -Headers @{ 'User-Agent' = 'wta-notice-gen' } -ErrorAction Stop
        if ($resp.StatusCode -eq 200) {
            return [PSCustomObject]@{
                Name = "SPDX:$Atom"
                Text = $resp.Content
            }
        }
    } catch {
        # 404 / network failure both fall through to "Full text could not be located".
    }
    return $null
}

# ---------------------------------------------------------------------------
# 5. License-text lookup chain: extracted src/ -> .crate tarball -> GitHub raw.
# ---------------------------------------------------------------------------
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $HOME '.cargo' }
$srcRoot   = Join-Path $cargoHome 'registry\src'
$cacheRoot = Join-Path $cargoHome 'registry\cache'

$licenseNames = @(
    'LICENSE','LICENSE.txt','LICENSE.md',
    'LICENSE-MIT','LICENSE-MIT.txt','LICENSE-MIT.md',
    'LICENSE-APACHE','LICENSE-APACHE.txt','LICENSE-APACHE.md',
    'LICENSE-MIT-Modern-Variant',
    'LICENSE-BSD','LICENSE-BSD.txt','LICENSE-BSD-3-Clause',
    'LICENSE-ISC','LICENSE-ISC.txt',
    'LICENSE-UNICODE','LICENSE-UNICODE.txt','LICENSE-UNICODE-LICENSE-V3',
    'LICENSE-ZLIB','LICENSE-ZLIB.txt',
    'LICENSE-BSL','LICENSE-BSL-1.0','LICENSE-BSL-1.0.txt',
    'COPYING','COPYING.txt','UNLICENSE','UNLICENSE.txt'
)
$licenseNamesLower = $licenseNames | ForEach-Object { $_.ToLower() }

function Get-LicenseFromDir {
    param([string]$Dir)
    $out = @()
    if (-not (Test-Path $Dir)) { return $out }
    foreach ($f in Get-ChildItem $Dir -File -ErrorAction SilentlyContinue) {
        if ($licenseNamesLower -contains $f.Name.ToLower()) {
            $out += [PSCustomObject]@{
                Name = $f.Name
                Text = [System.IO.File]::ReadAllText($f.FullName)
            }
        }
    }
    return $out
}

function Get-LicenseFromCrate {
    param([string]$Name, [string]$Version)
    $out = @()
    if (-not (Test-Path $cacheRoot)) { return $out }
    foreach ($reg in Get-ChildItem $cacheRoot -Directory -ErrorAction SilentlyContinue) {
        $crate = Join-Path $reg.FullName "$Name-$Version.crate"
        if (-not (Test-Path $crate)) { continue }
        $tmp = Join-Path ([System.IO.Path]::GetTempPath()) "wta-cg-$([Guid]::NewGuid().ToString('N'))"
        New-Item -ItemType Directory -Path $tmp -Force | Out-Null
        try {
            & tar.exe -xzf $crate -C $tmp 2>$null
            if ($LASTEXITCODE -eq 0) {
                $out = Get-LicenseFromDir (Join-Path $tmp "$Name-$Version")
            }
        } finally {
            Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
        }
        if ($out.Count -gt 0) { return $out }
    }
    return $out
}

function Get-LicenseFromGithub {
    param([string]$RepoUrl)
    if (-not $RepoUrl -or -not $RepoUrl.Contains('github.com')) { return $null }
    $url = $RepoUrl.TrimEnd('/') -replace '\.git$', ''
    $parts = $url -split '/'
    $gi = [array]::IndexOf($parts, 'github.com')
    if ($gi -lt 0 -or $parts.Count -lt $gi + 3) { return $null }
    $owner = $parts[$gi + 1]; $repo = $parts[$gi + 2]
    $base = "https://raw.githubusercontent.com/$owner/$repo/HEAD"
    foreach ($fname in $licenseNames) {
        try {
            $resp = Invoke-WebRequest -Uri "$base/$fname" -UseBasicParsing -TimeoutSec 15 `
                -Headers @{ 'User-Agent' = 'wta-notice-gen' } -ErrorAction Stop
            if ($resp.StatusCode -eq 200) {
                return [PSCustomObject]@{ Name = $fname; Text = $resp.Content }
            }
        } catch {
            $code = if ($_.Exception.Response) { [int]$_.Exception.Response.StatusCode } else { 0 }
            if ($code -ne 404) { return $null }
        }
    }
    return $null
}

function Get-LicenseText {
    param([string]$Name, [string]$Version, [string]$RepoUrl)
    if (Test-Path $srcRoot) {
        foreach ($reg in Get-ChildItem $srcRoot -Directory -ErrorAction SilentlyContinue) {
            $found = Get-LicenseFromDir (Join-Path $reg.FullName "$Name-$Version")
            if ($found.Count -gt 0) { return $found }
        }
    }
    $found = Get-LicenseFromCrate -Name $Name -Version $Version
    if ($found.Count -gt 0) { return $found }
    $upstream = Get-LicenseFromGithub -RepoUrl $RepoUrl
    if ($upstream) { return @($upstream) }
    return @()
}

# ---------------------------------------------------------------------------
# 6. Single pass: emit cgmanifest registrations + accumulate atom -> crates
#    mapping for the NOTICE block. Picks a representative license-text
#    source per (atom, crate) for later canonical-text emission.
# ---------------------------------------------------------------------------
$registrations = @()
$bulletLines = New-Object System.Collections.Generic.List[string]
$atomToCrates = @{}      # atom -> ordered list of "name vX.Y.Z"
$atomToTextCandidates = @{}  # atom -> list of [PSCustomObject @{Name; Text; FromCrate}]

foreach ($pkg in $attributed) {
    $name = $pkg.name
    $version = $pkg.version
    $rawSpdx = if ($pkg.license) { $pkg.license } else { '' }
    $normSpdx = Get-NormalizedSpdx -Raw $rawSpdx
    $atoms = Get-SpdxAtoms -Normalized $normSpdx
    $repo = if ($pkg.repository) { $pkg.repository }
            elseif ($pkg.homepage) { $pkg.homepage }
            else { "https://crates.io/crates/$name" }

    $bulletLines.Add("- **$name** v$version -- [$repo]($repo) -- ``$normSpdx``")

    $registrations += [PSCustomObject]@{
        component = [PSCustomObject]@{
            type  = 'cargo'
            cargo = [PSCustomObject]@{
                name    = $name
                version = $version
            }
        }
    }

    foreach ($atom in $atoms) {
        if (-not $atomToCrates.ContainsKey($atom)) { $atomToCrates[$atom] = @() }
        $atomToCrates[$atom] += "$name v$version"
    }
}

# ---------------------------------------------------------------------------
# 6a. Per-crate copyright harvesting. MIT / BSD / Apache-2.0 all require the
#     original "Copyright (c) <year> <holder>" line to travel with the
#     binary. SPDX-canonical license text has only placeholder copyright, so
#     we additionally scrape the actual lines out of each crate's LICENSE
#     file and emit them as a dedicated "Copyright notices" subsection under
#     each atom in the License Texts block.
# ---------------------------------------------------------------------------
$atomToCopyrights = @{}  # atom -> sorted unique list of copyright lines

$copyrightRegex = '^\s*(?:#\s*)?(?:Copyright(?:\s*\(c\))?|\(C\)|\(c\)|©)\s+(?:\d{4}[\d ,\-]*\s+).+$'

foreach ($atom in $atomToCrates.Keys) {
    $atomToCopyrights[$atom] = New-Object 'System.Collections.Generic.SortedSet[string]'
    foreach ($cratePair in $atomToCrates[$atom]) {
        if ($cratePair -notmatch '^(.+) v(.+)$') { continue }
        $cname = $matches[1]; $cver = $matches[2]
        $cratePkg = $attributed | Where-Object { $_.name -eq $cname -and $_.version -eq $cver } | Select-Object -First 1
        if (-not $cratePkg) { continue }
        $crepo = if ($cratePkg.repository) { $cratePkg.repository } else { "https://crates.io/crates/$cname" }
        $texts = Get-LicenseText -Name $cname -Version $cver -RepoUrl $crepo
        foreach ($t in $texts) {
            foreach ($line in ($t.Text -split "`r?`n")) {
                if ($line -match $copyrightRegex) {
                    [void]$atomToCopyrights[$atom].Add($line.Trim())
                }
            }
        }
    }
}

# ---------------------------------------------------------------------------
# 6b. License-text resolution per atom: SPDX project canonical first, then
#     per-crate file fallback. The SPDX text is the canonical license body;
#     the harvested copyright lines from 6a sit alongside it so MIT/BSD
#     attribution requirements are met without embedding a single crate's
#     bundled file as if it represented the canonical text for many.
# ---------------------------------------------------------------------------
foreach ($atom in $atomToCrates.Keys) {
    if ($atomToTextCandidates.ContainsKey($atom) -and $atomToTextCandidates[$atom].Count -gt 0) { continue }
    $atomToTextCandidates[$atom] = @()

    $spdx = Get-SpdxCanonicalText -Atom $atom
    if ($spdx) {
        $atomToTextCandidates[$atom] = @($spdx)
        continue
    }

    # Fall back to scanning crate files that declare this atom. Try each
    # crate's bundled LICENSE files; prefer filename matches that align
    # with the atom (e.g. LICENSE-MIT for the MIT atom).
    foreach ($cratePair in $atomToCrates[$atom]) {
        if ($cratePair -notmatch '^(.+) v(.+)$') { continue }
        $cname = $matches[1]; $cver = $matches[2]
        # Re-derive repo URL for the GitHub fallback inside Get-LicenseText.
        $cratePkg = $attributed | Where-Object { $_.name -eq $cname -and $_.version -eq $cver } | Select-Object -First 1
        if (-not $cratePkg) { continue }
        $crepo = if ($cratePkg.repository) { $cratePkg.repository } else { "https://crates.io/crates/$cname" }
        $texts = Get-LicenseText -Name $cname -Version $cver -RepoUrl $crepo
        if ($texts.Count -eq 0) { continue }

        $atomLower = $atom.ToLower()
        foreach ($t in $texts) {
            $fl = $t.Name.ToLower()
            $matched =
                ($atomLower.Contains('apache')   -and $fl.Contains('apache')) -or
                ($atomLower.Contains('mit')      -and $fl.Contains('mit') -and -not $fl.Contains('apache')) -or
                ($atomLower.Contains('bsd')      -and $fl.Contains('bsd')) -or
                ($atomLower.Contains('isc')      -and $fl.Contains('isc')) -or
                # Unicode-DFS-2016 vs Unicode-3.0: LICENSE-UNICODE typically holds V3 text.
                ($atom -eq 'Unicode-3.0'         -and $fl.Contains('unicode')) -or
                ($atomLower.Contains('zlib')     -and $fl.Contains('zlib')) -or
                ($atomLower.Contains('bsl')      -and ($fl.Contains('bsl') -or $fl.Contains('boost'))) -or
                ($atomLower.Contains('unlicense') -and $fl.Contains('unlicense')) -or
                ($atomLower.Contains('wtfpl')    -and $fl.Contains('wtfpl'))
            if ($matched) { $atomToTextCandidates[$atom] = @($t); break }
        }
        if ($atomToTextCandidates[$atom].Count -eq 0 -and $texts.Count -eq 1) {
            # Singly-licensed crate with a single LICENSE file — accept it.
            $atomToTextCandidates[$atom] = @($texts[0])
        }
        if ($atomToTextCandidates[$atom].Count -gt 0) { break }
    }
}

# Track atoms that still have no candidate.
$missingAtoms = @()
foreach ($atom in $atomToCrates.Keys) {
    if ($atomToTextCandidates[$atom].Count -eq 0) { $missingAtoms += $atom }
}

# ---------------------------------------------------------------------------
# 7. Write cgmanifest.json (sorted for stable diffs).
# ---------------------------------------------------------------------------
$cgManifest = [PSCustomObject]@{
    '$schema'     = 'https://json.schemastore.org/component-detection-manifest.json'
    Registrations = @($registrations | Sort-Object { $_.component.cargo.name }, { $_.component.cargo.version })
    Version       = 1
}
$cgJson = $cgManifest | ConvertTo-Json -Depth 10
[System.IO.File]::WriteAllText($cgManifestPath, $cgJson + "`n", [System.Text.UTF8Encoding]::new($false))
Write-Host "Wrote $cgManifestPath ($((Get-Item $cgManifestPath).Length) bytes, $($registrations.Count) registrations)" -ForegroundColor Green

# ---------------------------------------------------------------------------
# 8. Build the NOTICE marker block. The block starts directly with the BEGIN
#    marker -- no leading blank line -- so successive regenerations don't
#    accumulate empty lines between the horizontal-rule separator above and
#    the marker.
# ---------------------------------------------------------------------------
$out = New-Object System.Collections.Generic.List[string]
$out.Add('<!-- BEGIN wta-rust-deps (auto-generated by build/scripts/Generate-WtaThirdPartyNotices.ps1) -->')
$out.Add('<!--')
$out.Add("Sourced from cargo metadata on $targetTriple; the script also writes")
$out.Add('tools/wta/cgmanifest.json for Microsoft Component Governance. Regenerate')
$out.Add('after dependency changes -- the script runs cargo metadata, generates both')
$out.Add('artifacts, and splices this block back into NOTICE.md in place.')
$out.Add('-->')
$out.Add('')
$out.Add('# Windows Terminal Agent (`tools/wta`) third-party Rust crates')
$out.Add('')
$out.Add("The following Rust crates are statically linked into ``wta.exe`` on ``$targetTriple``.")
$out.Add('Each crate is listed with its source URL and the SPDX license expression from')
$out.Add('its Cargo manifest. Canonical license texts are bundled in')
$out.Add('[License texts](#license-texts) below, one section per atomic SPDX')
$out.Add('identifier (composite expressions like `(MIT OR Apache-2.0) AND Unicode-3.0` are')
$out.Add('decomposed so every required text is reproduced exactly once). Per-crate')
$out.Add('copyright lines harvested from each crate''s bundled LICENSE file are listed')
$out.Add('under each atom alongside its canonical text, so MIT / BSD attribution')
$out.Add('requirements are met without embedding any single crate''s license body as')
$out.Add('if it were canonical for the whole group.')
$out.Add('')
foreach ($line in $bulletLines) { $out.Add($line) }
$out.Add('')
$out.Add('## License texts')
$out.Add('')
$out.Add('One section per unique atomic SPDX identifier that appears anywhere in the')
$out.Add('above crate list (directly or as a component of a composite expression).')
$out.Add('Each section reproduces the SPDX-canonical text of the license and then,')
$out.Add('where applicable, the per-crate copyright notices harvested from upstream.')
$out.Add('')

foreach ($atom in ($atomToCrates.Keys | Sort-Object)) {
    $cratesForAtom = $atomToCrates[$atom] | Sort-Object -Unique
    $out.Add("### ``$atom``")
    $out.Add('')
    $head = $cratesForAtom | Select-Object -First 8
    $applies = "Applies to $($cratesForAtom.Count) crate(s) (directly or via composite identifiers): " + ($head -join ', ')
    if ($cratesForAtom.Count -gt 8) { $applies += ", ... (+$($cratesForAtom.Count - 8) more)" }
    $out.Add($applies)
    $out.Add('')
    $candidate = $atomToTextCandidates[$atom]
    if ($candidate -and $candidate.Count -gt 0) {
        $src = $candidate[0]
        $out.Add("_Canonical text reproduced from upstream ``$($src.Name)``:_")
        $out.Add('')
        $out.Add('```')
        $out.Add($src.Text.TrimEnd())
        $out.Add('```')
    } else {
        $out.Add('```')
        $out.Add("License: $atom")
        $out.Add('(Full text could not be located in the Cargo cache or upstream;')
        $out.Add(' consult any of the source URLs listed above for the canonical text.)')
        $out.Add('```')
    }

    # Per-crate copyright notices harvested from real LICENSE files. Required
    # by MIT / BSD / Apache-2.0 to preserve attribution in the distributed
    # binary; the SPDX-canonical text above only carries placeholder copyright.
    $copyrights = $atomToCopyrights[$atom]
    if ($copyrights -and $copyrights.Count -gt 0) {
        $out.Add('')
        $out.Add("_Copyright notices harvested from the above crates' upstream LICENSE files:_")
        $out.Add('')
        $out.Add('```')
        foreach ($c in $copyrights) { $out.Add($c) }
        $out.Add('```')
    }
    $out.Add('')
}

$block = ($out -join "`n") + "`n<!-- END wta-rust-deps -->"

# ---------------------------------------------------------------------------
# 9. Splice into NOTICE.md, preserving the original CRLF line endings.
# ---------------------------------------------------------------------------
$origBytes = [System.IO.File]::ReadAllBytes($noticePath)
$origText  = [System.Text.Encoding]::UTF8.GetString($origBytes)
$crlfCount = ([regex]::Matches($origText, "`r`n")).Count
$lfCount   = ([regex]::Matches($origText, "(?<!`r)`n")).Count
$usesCrlf  = ($crlfCount -gt 0) -and ($crlfCount -gt $lfCount)

$origLf = $origText -replace "`r`n", "`n"

# The marker pattern intentionally consumes any blank lines preceding the
# BEGIN marker so successive regenerations collapse accumulated whitespace
# instead of letting it pile up between the horizontal-rule separator and
# the marker block. The replacement always re-emits exactly "\n\n-----\n"
# before the block (matching the first-append form), so the file converges
# on a stable canonical layout no matter what state it started in.
$markerPattern = '(?s)(?:\r?\n)*(?:-----\s*(?:\r?\n)+)?<!-- BEGIN wta-rust-deps[^>]*-->.*?<!-- END wta-rust-deps -->'
$canonicalBlock = "`n`n-----`n" + $block
if ([regex]::IsMatch($origLf, $markerPattern)) {
    # MatchEvaluator avoids `$0` / `$1` substitution if license text contains $-refs.
    $updatedLf = [regex]::Replace($origLf.TrimEnd("`n"), $markerPattern,
        [System.Text.RegularExpressions.MatchEvaluator]{ param($m) $canonicalBlock }) + "`n"
    $action = 'replaced marker block'
} else {
    $updatedLf = $origLf.TrimEnd("`n") + $canonicalBlock + "`n"
    $action = 'appended new marker block'
}

$outText = if ($usesCrlf) { $updatedLf -replace "`n", "`r`n" } else { $updatedLf }
$outBytes = [System.Text.UTF8Encoding]::new($false).GetBytes($outText)

# Atomic write: same-directory temp file + Move-Item -Force.
$tmpPath = "$noticePath.$([Guid]::NewGuid().ToString('N')).tmp"
try {
    [System.IO.File]::WriteAllBytes($tmpPath, $outBytes)
    Move-Item -Path $tmpPath -Destination $noticePath -Force
} catch {
    Remove-Item $tmpPath -Force -ErrorAction SilentlyContinue
    throw
}

Write-Host "$action in NOTICE.md: $($origBytes.Length) -> $($outBytes.Length) bytes (CRLF=$usesCrlf)" -ForegroundColor Green
Write-Host "  $($attributed.Count) crates, $($atomToCrates.Count) unique atomic licenses" -ForegroundColor Green
if ($missingAtoms.Count -gt 0) {
    # Fail hard: a partially-attributed NOTICE is a worse outcome than a build
    # break, because it can silently land in the package as incomplete legal
    # attribution. Force the regenerator to deal with the gap before commit.
    $list = ($missingAtoms | Select-Object -First 20) -join ', '
    throw "$($missingAtoms.Count) atomic license(s) had no canonical text candidate (resolve before committing): $list"
}
