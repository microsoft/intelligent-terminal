[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidateNotNullOrEmpty()]
    [string]$PackageRoot
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$expectedPackageName = 'Microsoft.Windows.ImplementationLibrary.1.0.250325.1'
$packagePath = (Resolve-Path -LiteralPath $PackageRoot).Path
if ((Split-Path -Leaf $packagePath) -cne $expectedPackageName)
{
    throw "Expected the $expectedPackageName package directory, but received '$packagePath'."
}

$optimizationOff = @'
#if defined(_MSC_FULL_VER) && !defined(__clang__) && _MSC_FULL_VER == 195236725
#pragma optimize("", off)
#endif
'@

$optimizationOn = @'
#if defined(_MSC_FULL_VER) && !defined(__clang__) && _MSC_FULL_VER == 195236725
#pragma optimize("", on)
#endif
'@

function Get-OccurrenceCount
{
    param(
        [Parameter(Mandatory)]
        [string]$Text,

        [Parameter(Mandatory)]
        [string]$Value
    )

    return [regex]::Matches($Text, [regex]::Escape($Value)).Count
}

function Update-WilExceptionConverter
{
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [Parameter(Mandatory)]
        [string]$Definition,

        [Parameter(Mandatory)]
        [string]$EndAnchor
    )

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf))
    {
        throw "Expected WIL header '$Path' was not found."
    }

    $bytes = [System.IO.File]::ReadAllBytes($Path)
    $hasUtf8Bom = $bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF
    $content = [System.Text.Encoding]::UTF8.GetString($bytes, $(if ($hasUtf8Bom) { 3 } else { 0 }), $bytes.Length - $(if ($hasUtf8Bom) { 3 } else { 0 }))
    $newLine = if ($content.Contains("`r`n")) { "`r`n" } else { "`n" }
    $EndAnchor = ($EndAnchor -replace "`r`n", "`n").Replace("`n", $newLine)
    $offMarker = (($optimizationOff -replace "`r`n", "`n").Replace("`n", $newLine)) + $newLine
    $onMarker = ($optimizationOn -replace "`r`n", "`n").Replace("`n", $newLine)

    $offCount = Get-OccurrenceCount -Text $content -Value $offMarker
    $onCount = Get-OccurrenceCount -Text $content -Value $onMarker
    if (($offCount -eq 1) -and ($onCount -eq 1))
    {
        if (($content.IndexOf($offMarker + $Definition, [System.StringComparison]::Ordinal) -lt 0) -or
            ($content.IndexOf($EndAnchor + $newLine + $onMarker, [System.StringComparison]::Ordinal) -lt 0))
        {
            throw "WIL header '$Path' contains an incomplete or misplaced optimization workaround."
        }

        Write-Host "Scoped WIL exception optimization workaround is already present in '$Path'."
        return
    }

    if (($offCount -ne 0) -or ($onCount -ne 0))
    {
        throw "WIL header '$Path' contains a partial optimization workaround."
    }

    if ((Get-OccurrenceCount -Text $content -Value $Definition) -ne 1)
    {
        throw "WIL header '$Path' does not contain exactly one expected converter definition."
    }

    if ((Get-OccurrenceCount -Text $content -Value $EndAnchor) -ne 1)
    {
        throw "WIL header '$Path' does not contain exactly one expected converter end."
    }

    $content = $content.Replace($Definition, $offMarker + $Definition)
    $content = $content.Replace($EndAnchor, $EndAnchor + $newLine + $onMarker)
    [System.IO.File]::WriteAllText($Path, $content, [System.Text.UTF8Encoding]::new($hasUtf8Bom))
    Write-Host "Applied scoped WIL exception optimization workaround to '$Path'."
}

$mutex = [System.Threading.Mutex]::new($false, 'Local\IntelligentTerminal.WilExceptionOptimizationWorkaround')
$lockTaken = $false
try
{
    try
    {
        $lockTaken = $mutex.WaitOne([TimeSpan]::FromMinutes(2))
    }
    catch [System.Threading.AbandonedMutexException]
    {
        $lockTaken = $true
        Write-Warning 'A previous WIL patch operation was interrupted; validating the headers before continuing.'
    }
    if (-not $lockTaken)
    {
        throw 'Timed out waiting to patch the WIL exception converters.'
    }

    $wilIncludePath = Join-Path $packagePath 'include\wil'
    Update-WilExceptionConverter `
        -Path (Join-Path $wilIncludePath 'cppwinrt.h') `
        -Definition 'inline HRESULT __stdcall ResultFromCaughtException_CppWinRt(' `
        -EndAnchor @'
    // Tell the caller that we were unable to map the exception by succeeding...
    return S_OK;
}
'@
    Update-WilExceptionConverter `
        -Path (Join-Path $wilIncludePath 'result_macros.h') `
        -Definition '    __declspec(noinline) inline ResultStatus __stdcall ResultFromCaughtExceptionInternal(' `
        -EndAnchor @'
        // Tell the caller that we were unable to map the exception by succeeding...
        return ResultStatus::FromResult(S_OK);
    }
'@
}
finally
{
    if ($lockTaken)
    {
        $mutex.ReleaseMutex()
    }

    $mutex.Dispose()
}
