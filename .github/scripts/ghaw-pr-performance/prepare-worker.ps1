$ErrorActionPreference = 'Stop'

if ($env:PR_NUMBER -notmatch '^[1-9][0-9]*$') {
    throw 'PR_NUMBER must be a positive decimal number.'
}
foreach ($name in @('BASE_SHA', 'HEAD_SHA', 'EXPECTED_BASE_SHA', 'WORKFLOW_SHA')) {
    if ([Environment]::GetEnvironmentVariable($name) -notmatch '^[0-9a-f]{40}$') {
        throw "$name must be an immutable lowercase commit SHA."
    }
}
if ($env:REPOSITORY -cne $env:GITHUB_REPOSITORY) {
    throw 'Dispatch repository must match the workflow repository.'
}
if ($env:SAME_REPO -notin @('true', 'false')) {
    throw 'SAME_REPO must be true or false.'
}

$metadataPath = Join-Path $env:RUNNER_TEMP 'performance-live-pr.json'
$metadata = & gh api "/repos/$env:REPOSITORY/pulls/$env:PR_NUMBER"
if ($LASTEXITCODE -ne 0) { throw 'Could not read the live PR metadata.' }
[IO.File]::WriteAllText($metadataPath, ($metadata -join "`n"), [Text.UTF8Encoding]::new($false))
& node .github/skills/pr-performance-review/scripts/performance-review.mjs verify-pr `
    --input $metadataPath --pr $env:PR_NUMBER --base $env:BASE_SHA --head $env:HEAD_SHA `
    --expected-base $env:EXPECTED_BASE_SHA --repo $env:REPOSITORY `
    --head-repo $env:HEAD_REPO --same-repo $env:SAME_REPO `
    --head-ref $env:HEAD_REF --base-ref $env:BASE_REF
if ($LASTEXITCODE -ne 0) { throw 'PR metadata does not match the immutable dispatch.' }

$remoteRef = "refs/remotes/origin/performance-pr-$env:PR_NUMBER"
& git -c credential.helper= -c 'credential.helper=!gh auth git-credential' `
    fetch --no-tags origin "refs/pull/$env:PR_NUMBER/head:$remoteRef"
if ($LASTEXITCODE -ne 0) { throw 'Could not fetch the immutable PR head.' }
$head = & git rev-parse $remoteRef
if ($LASTEXITCODE -ne 0 -or $head -cne $env:HEAD_SHA) { throw 'The fetched PR head is stale.' }
$mergeBase = & git merge-base $env:EXPECTED_BASE_SHA $env:HEAD_SHA
if ($LASTEXITCODE -ne 0 -or $mergeBase -cne $env:BASE_SHA) {
    throw 'The dispatched comparison base is not the merge base.'
}
$changeSummary = (& git diff --no-ext-diff --no-textconv --shortstat --no-renames $env:BASE_SHA $env:HEAD_SHA -- | Out-String).Trim()
if ($LASTEXITCODE -ne 0) { throw 'Could not summarize the immutable PR change size.' }
"change_summary=$changeSummary" >> $env:GITHUB_OUTPUT

if ($env:SAME_REPO -eq 'true') {
    $context = $env:AW_CONTEXT | ConvertFrom-Json
    if ($context.item_type -cne 'pull_request' -or $context.item_number -ne [int]$env:PR_NUMBER -or
        $context.repo -cne $env:REPOSITORY -or $context.head_sha -cne $env:HEAD_SHA) {
        throw 'Repair requires the controller-supplied native aw_context for this exact PR.'
    }
}
"trusted_code_revision=$env:WORKFLOW_SHA" >> $env:GITHUB_OUTPUT
