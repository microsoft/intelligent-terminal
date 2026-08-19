# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.

$ErrorActionPreference = 'Stop'
$env:GH_PROMPT_DISABLED = '1'
$env:GH_PAGER = 'cat'
$env:PAGER = 'cat'

try {
    $requestText = [Console]::In.ReadToEnd()
    $request = $requestText | ConvertFrom-Json -Depth 16
    $requestId = [string]$request.requestId
    if ([string]::IsNullOrWhiteSpace($requestId)) {
        throw 'requestId is required'
    }

    $workingDirectory = [string]$request.params.workingDirectory.value
    if ([string]::IsNullOrWhiteSpace($workingDirectory) -or -not (Test-Path -LiteralPath $workingDirectory -PathType Container)) {
        $response = @{
            protocolVersion = 1
            requestId = $requestId
            result = @{ fields = @{} }
        }
    }
    else {
        Push-Location -LiteralPath $workingDirectory
        try {
            $json = & gh pr view --json number,state,isDraft,statusCheckRollup,url 2>$null
            if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($json)) {
                $response = @{
                    protocolVersion = 1
                    requestId = $requestId
                    result = @{ fields = @{} }
                }
            }
            else {
                $pr = $json | ConvertFrom-Json -Depth 32
                $failed = @($pr.statusCheckRollup | Where-Object {
                    $_.conclusion -in @('FAILURE', 'CANCELLED', 'TIMED_OUT', 'ACTION_REQUIRED')
                }).Count
                $pending = @($pr.statusCheckRollup | Where-Object {
                    [string]::IsNullOrWhiteSpace([string]$_.conclusion) -or $_.status -ne 'COMPLETED'
                }).Count
                $state = if ($pr.isDraft) { 'DRAFT' } else { [string]$pr.state }
                $checks = if ($failed -gt 0) {
                    "checks failed:$failed"
                }
                elseif ($pending -gt 0) {
                    "checks pending:$pending"
                }
                else {
                    'checks passing'
                }

                $response = @{
                    protocolVersion = 1
                    requestId = $requestId
                    result = @{
                        fields = @{
                            'pull-request' = "PR #$($pr.number)"
                            state = $state
                            checks = $checks
                        }
                        tooltip = [string]$pr.url
                        accessibilityText = "Pull request $($pr.number), $state, $checks"
                    }
                }
            }
        }
        finally {
            Pop-Location
        }
    }

    [Console]::Out.Write(($response | ConvertTo-Json -Compress -Depth 16))
}
catch {
    [Console]::Error.WriteLine($_.Exception.Message)
    exit 1
}
