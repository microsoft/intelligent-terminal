$PackageRoot = $env:WIL_EXCEPTION_OPTIMIZATION_WORKAROUND_PACKAGE_ROOT
if ([string]::IsNullOrWhiteSpace($PackageRoot))
{
    throw 'Set WIL_EXCEPTION_OPTIMIZATION_WORKAROUND_PACKAGE_ROOT to the restored WIL package directory.'
}

[Environment]::SetEnvironmentVariable(
    'WIL_EXCEPTION_OPTIMIZATION_WORKAROUND_SCRIPT_PATH',
    (Join-Path $PSScriptRoot '..\Patch-WilExceptionOptimizationWorkaround.ps1'),
    'Process')

Describe 'Patch-WilExceptionOptimizationWorkaround' {
    BeforeEach {
        $sourcePackageRoot = [Environment]::GetEnvironmentVariable('WIL_EXCEPTION_OPTIMIZATION_WORKAROUND_PACKAGE_ROOT', 'Process')
        $testPackageRoot = Join-Path (Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))) 'Microsoft.Windows.ImplementationLibrary.1.0.250325.1'
        $includePath = Join-Path $testPackageRoot 'include\wil'
        New-Item -ItemType Directory -Path $includePath -Force | Out-Null
        foreach ($header in @('cppwinrt.h', 'result_macros.h'))
        {
            Copy-Item -LiteralPath (Join-Path $sourcePackageRoot "include\wil\$header") -Destination $includePath
        }
    }

    It 'scopes the workaround to the two affected converter definitions' {
        $packageRoot = $testPackageRoot
        $scriptPath = [Environment]::GetEnvironmentVariable('WIL_EXCEPTION_OPTIMIZATION_WORKAROUND_SCRIPT_PATH', 'Process')
        $cppWinRtHeader = Join-Path $packageRoot 'include\wil\cppwinrt.h'
        $resultMacrosHeader = Join-Path $packageRoot 'include\wil\result_macros.h'
        & $scriptPath -PackageRoot $packageRoot

        $guard = '#if defined(_MSC_FULL_VER) && !defined(__clang__) && _MSC_FULL_VER == 195236725'
        $pragmaOff = '#pragma optimize("", off)'
        $pragmaOn = '#pragma optimize("", on)'
        $cppWinRt = Get-Content -LiteralPath $cppWinRtHeader -Raw
        $resultMacros = Get-Content -LiteralPath $resultMacrosHeader -Raw
        $newLine = if ($cppWinRt.Contains("`r`n")) { "`r`n" } else { "`n" }

        $cppWinRt.Contains("$guard$newLine$pragmaOff$newLine#endif$newLine" + 'inline HRESULT __stdcall ResultFromCaughtException_CppWinRt(') | Should -BeTrue
        $cppWinRt.Contains("return S_OK;$newLine}$newLine$guard$newLine$pragmaOn$newLine#endif") | Should -BeTrue
        $resultMacros.Contains("$guard$newLine$pragmaOff$newLine#endif$newLine" + '    __declspec(noinline) inline ResultStatus __stdcall ResultFromCaughtExceptionInternal(') | Should -BeTrue
        $resultMacros.Contains("return ResultStatus::FromResult(S_OK);$newLine    }$newLine$guard$newLine$pragmaOn$newLine#endif") | Should -BeTrue
        ([regex]::Matches($cppWinRt + $resultMacros, '#pragma optimize\("", (off|on)\)')).Count | Should -Be 4
    }

    It 'is idempotent' {
        $packageRoot = $testPackageRoot
        $scriptPath = [Environment]::GetEnvironmentVariable('WIL_EXCEPTION_OPTIMIZATION_WORKAROUND_SCRIPT_PATH', 'Process')
        $cppWinRtHeader = Join-Path $packageRoot 'include\wil\cppwinrt.h'
        $resultMacrosHeader = Join-Path $packageRoot 'include\wil\result_macros.h'
        & $scriptPath -PackageRoot $packageRoot
        $before = @(
            Get-FileHash -LiteralPath $cppWinRtHeader -Algorithm SHA256
            Get-FileHash -LiteralPath $resultMacrosHeader -Algorithm SHA256
        )

        & $scriptPath -PackageRoot $packageRoot

        $after = @(
            Get-FileHash -LiteralPath $cppWinRtHeader -Algorithm SHA256
            Get-FileHash -LiteralPath $resultMacrosHeader -Algorithm SHA256
        )
        $after.Hash | Should -Be $before.Hash
    }

    It 'fails hard for an invalid package directory' {
        $packageRoot = $testPackageRoot
        $scriptPath = [Environment]::GetEnvironmentVariable('WIL_EXCEPTION_OPTIMIZATION_WORKAROUND_SCRIPT_PATH', 'Process')
        { & $scriptPath -PackageRoot (Join-Path $packageRoot 'not-the-wil-package') } | Should -Throw
    }

    It 'fails hard when a converter definition is malformed' {
        $packageRoot = $testPackageRoot
        $scriptPath = [Environment]::GetEnvironmentVariable('WIL_EXCEPTION_OPTIMIZATION_WORKAROUND_SCRIPT_PATH', 'Process')
        $cppWinRtHeader = Join-Path $packageRoot 'include\wil\cppwinrt.h'
        $originalContent = Get-Content -LiteralPath $cppWinRtHeader -Raw
        $malformedContent = $originalContent.Replace(
            'inline HRESULT __stdcall ResultFromCaughtException_CppWinRt(',
            'inline HRESULT __stdcall UnexpectedConverter(')

        try
        {
            [System.IO.File]::WriteAllText($cppWinRtHeader, $malformedContent, [System.Text.UTF8Encoding]::new($false))
            { & $scriptPath -PackageRoot $packageRoot } | Should -Throw
        }
        finally
        {
            [System.IO.File]::WriteAllText($cppWinRtHeader, $originalContent, [System.Text.UTF8Encoding]::new($false))
        }
    }
}
