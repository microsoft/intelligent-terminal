// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// PowerShellProfileRepair.h
//
// Fail-closed relocation of the Intelligent Terminal managed profile block
// after a statically recognized Oh My Posh initializer.

#pragma once

#include "PowerShellShellIntegration.h"

#include <algorithm>
#include <cstdint>
#include <cwctype>
#include <string>
#include <string_view>
#include <vector>

namespace Microsoft::Terminal::ShellIntegration::Powershell
{
    enum class ProfileRepairOutcome
    {
        Repaired,
        RepairedWithRecoveryBackup,
        NotApplicable,
        Unsupported,
        Failed,
    };

    struct ProfileRepairResult
    {
        ProfileRepairOutcome outcome{ ProfileRepairOutcome::Failed };
        std::wstring errorMessage;
    };

    namespace repair_details
    {
        inline std::wstring Base64Encode(const uint8_t* data, const size_t size)
        {
            static constexpr wchar_t alphabet[] =
                L"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            std::wstring encoded;
            encoded.reserve(((size + 2) / 3) * 4);
            for (size_t i = 0; i < size; i += 3)
            {
                const uint32_t a = data[i];
                const uint32_t b = i + 1 < size ? data[i + 1] : 0;
                const uint32_t c = i + 2 < size ? data[i + 2] : 0;
                const uint32_t value = (a << 16) | (b << 8) | c;
                encoded.push_back(alphabet[(value >> 18) & 0x3f]);
                encoded.push_back(alphabet[(value >> 12) & 0x3f]);
                encoded.push_back(i + 1 < size ? alphabet[(value >> 6) & 0x3f] : L'=');
                encoded.push_back(i + 2 < size ? alphabet[value & 0x3f] : L'=');
            }
            return encoded;
        }

        inline std::wstring Base64EncodeUtf16(std::wstring_view value)
        {
            return Base64Encode(
                reinterpret_cast<const uint8_t*>(value.data()),
                value.size() * sizeof(wchar_t));
        }

        inline constexpr std::wstring_view RepairScript = LR"PS(
$ErrorActionPreference = 'Stop'
$outcome = 13
$stream = $null
$liveStream = $null
$locked = $false
$liveLocked = $false
$originalBytes = $null
$tempPath = $null
$replaceBackup = $null
$committed = $false
$failureType = ''
$failureHResult = ''
$failureStage = 'startup'
$recoveryFailedAfterCommit = $false
$committedProfileMatches = $false

function Complete([int]$code) {
    [Console]::Out.WriteLine("ITPROFILE-$code|$failureStage|$failureType|$failureHResult")
    exit $code
}

function StopRepair([int]$code) {
    $exception = New-Object System.InvalidOperationException('Profile repair stopped')
    $exception.Data['ITProfileOutcome'] = $code
    throw $exception
}

function StaticString($ast) {
    if ($ast -is [System.Management.Automation.Language.StringConstantExpressionAst]) {
        return $ast.Value
    }
    return $null
}

function IsOmpInitializer($statement) {
    if (-not ($statement -is [System.Management.Automation.Language.PipelineAst]) -or
        $statement.PipelineElements.Count -ne 2) {
        return $false
    }

    $right = $statement.PipelineElements[1]
    if (-not ($right -is [System.Management.Automation.Language.CommandAst]) -or
        $right.Redirections.Count -ne 0 -or
        $right.CommandElements.Count -ne 1) {
        return $false
    }
    $rightName = $right.GetCommandName()
    if ($null -eq $rightName -or
        ($rightName -ine 'Invoke-Expression' -and $rightName -ine 'iex')) {
        return $false
    }

    $leftCommands = @($statement.PipelineElements[0].FindAll({
        param($node)
        $node -is [System.Management.Automation.Language.CommandAst]
    }, $true))
    if ($leftCommands.Count -ne 1) {
        return $false
    }

    $left = $leftCommands[0]
    if ($left.Redirections.Count -ne 0 -or $left.CommandElements.Count -lt 3) {
        return $false
    }
    $leftName = $left.GetCommandName()
    if ($null -eq $leftName) {
        return $false
    }
    $leaf = [System.IO.Path]::GetFileName($leftName)
    if ($leaf.EndsWith('.exe', [System.StringComparison]::OrdinalIgnoreCase)) {
        $leaf = $leaf.Substring(0, $leaf.Length - 4)
    }
    if ($leaf -ine 'oh-my-posh') {
        return $false
    }

    $verb = StaticString $left.CommandElements[1]
    $shell = StaticString $left.CommandElements[2]
    return $verb -ieq 'init' -and ($shell -ieq 'pwsh' -or $shell -ieq 'powershell')
}

try {
    if ([string]::IsNullOrWhiteSpace($path64)) {
        StopRepair 13
    }
    $path = [System.Text.Encoding]::Unicode.GetString(
        [Convert]::FromBase64String($path64))

    $failureStage = 'open'
    $stream = New-Object System.IO.FileStream(
        $path,
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Read,
        ([System.IO.FileShare]::Read -bor [System.IO.FileShare]::Delete))
    $failureStage = 'lock'
    $stream.Lock(0, [long]::MaxValue)
    $locked = $true

    if ($stream.Length -gt 1048576) {
        StopRepair 12
    }
    $originalBytes = New-Object byte[] ([int]$stream.Length)
    $offset = 0
    while ($offset -lt $originalBytes.Length) {
        $read = $stream.Read($originalBytes, $offset, $originalBytes.Length - $offset)
        if ($read -le 0) {
            throw 'Unexpected end of profile'
        }
        $offset += $read
    }

    $bomLength = 0
    $encoding = $null
    if ($originalBytes.Length -ge 3 -and
        $originalBytes[0] -eq 0xEF -and
        $originalBytes[1] -eq 0xBB -and
        $originalBytes[2] -eq 0xBF) {
        $bomLength = 3
        $encoding = New-Object System.Text.UTF8Encoding($false, $true)
    }
    elseif ($originalBytes.Length -ge 2 -and
            $originalBytes[0] -eq 0xFF -and
            $originalBytes[1] -eq 0xFE) {
        $bomLength = 2
        $encoding = New-Object System.Text.UnicodeEncoding($false, $false, $true)
    }
    elseif ($originalBytes.Length -ge 2 -and
            $originalBytes[0] -eq 0xFE -and
            $originalBytes[1] -eq 0xFF) {
        $bomLength = 2
        $encoding = New-Object System.Text.UnicodeEncoding($true, $false, $true)
    }
    else {
        foreach ($value in $originalBytes) {
            if ($value -gt 0x7F) {
                StopRepair 12
            }
        }
        $encoding = New-Object System.Text.UTF8Encoding($false, $true)
    }

    $payloadLength = $originalBytes.Length - $bomLength
    $payload = New-Object byte[] $payloadLength
    if ($payloadLength -gt 0) {
        [Array]::Copy($originalBytes, $bomLength, $payload, 0, $payloadLength)
    }
    $text = $encoding.GetString($payload)

    $openMarker = '# >>> intelligent-terminal shell-integration >>>'
    $closeMarker = '# <<< intelligent-terminal shell-integration <<<'
    $blockStart = $text.IndexOf($openMarker, [System.StringComparison]::Ordinal)
    $closeStart = $text.IndexOf($closeMarker, [System.StringComparison]::Ordinal)
    if ($blockStart -lt 0 -or $closeStart -lt $blockStart -or
        $text.LastIndexOf($openMarker, [System.StringComparison]::Ordinal) -ne $blockStart -or
        $text.LastIndexOf($closeMarker, [System.StringComparison]::Ordinal) -ne $closeStart) {
        StopRepair 12
    }
    if (($blockStart -gt 0 -and $text[$blockStart - 1] -ne "`n") -or
        ($closeStart -gt 0 -and $text[$closeStart - 1] -ne "`n")) {
        StopRepair 12
    }

    $closeEnd = $closeStart + $closeMarker.Length
    $closeLineEnd = $text.IndexOf("`n", $closeEnd)
    if ($closeLineEnd -lt 0) {
        StopRepair 12
    }
    $closeTail = $text.Substring($closeEnd, $closeLineEnd - $closeEnd)
    if (-not [string]::IsNullOrWhiteSpace($closeTail)) {
        StopRepair 12
    }
    $moveEnd = $closeLineEnd + 1

    $tokens = $null
    $parseErrors = $null
    $ast = [System.Management.Automation.Language.Parser]::ParseInput(
        $text,
        [ref]$tokens,
        [ref]$parseErrors)
    if ($parseErrors.Count -ne 0) {
        StopRepair 12
    }

    $statements = @($ast.EndBlock.Statements)
    $ompStatements = @($statements | Where-Object { IsOmpInitializer $_ })
    if ($ompStatements.Count -ne 1) {
        StopRepair 12
    }
    $omp = $ompStatements[0]
    if ($omp.Extent.StartOffset -lt $moveEnd) {
        StopRepair 11
    }

    $afterBlock = @($statements | Where-Object {
        $_.Extent.StartOffset -ge $moveEnd
    })
    if ($afterBlock.Count -eq 0 -or
        $afterBlock[0].Extent.StartOffset -ne $omp.Extent.StartOffset) {
        StopRepair 12
    }

    $ompLineEnd = $text.IndexOf("`n", $omp.Extent.EndOffset)
    if ($ompLineEnd -lt 0) {
        StopRepair 12
    }
    if ($afterBlock.Count -gt 1 -and
        $afterBlock[1].Extent.StartOffset -lt ($ompLineEnd + 1)) {
        StopRepair 12
    }
    $ompTail = $text.Substring(
        $omp.Extent.EndOffset,
        $ompLineEnd - $omp.Extent.EndOffset)
    if (-not [string]::IsNullOrWhiteSpace(
            ($ompTail -replace '#.*$', ''))) {
        StopRepair 12
    }

    $movedBlock = $text.Substring($blockStart, $moveEnd - $blockStart)
    $withoutBlock = $text.Remove($blockStart, $moveEnd - $blockStart)
    $insertAt = ($ompLineEnd + 1) - ($moveEnd - $blockStart)
    if ($insertAt -lt 0 -or $insertAt -gt $withoutBlock.Length) {
        StopRepair 13
    }
    $newText = $withoutBlock.Insert($insertAt, $movedBlock)
    if ($newText -eq $text) {
        StopRepair 11
    }

    $encodedBody = $encoding.GetBytes($newText)
    $newBytes = New-Object byte[] ($bomLength + $encodedBody.Length)
    if ($bomLength -gt 0) {
        [Array]::Copy($originalBytes, 0, $newBytes, 0, $bomLength)
    }
    [Array]::Copy($encodedBody, 0, $newBytes, $bomLength, $encodedBody.Length)

    $backup = $path + '.bak.it-repair.' +
        [DateTime]::UtcNow.ToString('yyyyMMdd-HHmmss') + '.' +
        [Guid]::NewGuid().ToString('N')
    $failureStage = 'backup'
    [System.IO.File]::WriteAllBytes($backup, $originalBytes)
    if (-not [System.IO.File]::Exists($backup)) {
        throw 'Backup was not created'
    }

    $failureStage = 'temp-write'
    $tempPath = $path + '.tmp.it-repair.' + [Guid]::NewGuid().ToString('N')
    [System.IO.File]::WriteAllBytes($tempPath, $newBytes)
    $tempBytes = [System.IO.File]::ReadAllBytes($tempPath)
    if ($tempBytes.Length -ne $newBytes.Length) {
        throw 'Temporary profile length mismatch'
    }
    for ($index = 0; $index -lt $newBytes.Length; $index++) {
        if ($tempBytes[$index] -ne $newBytes[$index]) {
            throw 'Temporary profile verification failed'
        }
    }
    $failureStage = 'unlock'
    $stream.Unlock(0, [long]::MaxValue)
    $locked = $false

    $failureStage = 'compare'
    $currentBytes = [System.IO.File]::ReadAllBytes($path)
    if ($currentBytes.Length -ne $originalBytes.Length) {
        StopRepair 12
    }
    for ($index = 0; $index -lt $originalBytes.Length; $index++) {
        if ($currentBytes[$index] -ne $originalBytes[$index]) {
            StopRepair 12
        }
    }
    $failureStage = 'replace'
    $replaceBackup = $backup + '.replace'
    [System.IO.File]::Replace($tempPath, $path, $replaceBackup, $true)
    $tempPath = $null
    $committed = $true

    $failureStage = 'lock-committed-profile'
    $liveStream = New-Object System.IO.FileStream(
        $path,
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Read,
        ([System.IO.FileShare]::Read -bor [System.IO.FileShare]::Delete))
    $liveStream.Lock(0, [long]::MaxValue)
    $liveLocked = $true
    $liveBytes = New-Object byte[] ([int]$liveStream.Length)
    $liveOffset = 0
    while ($liveOffset -lt $liveBytes.Length) {
        $liveRead = $liveStream.Read(
            $liveBytes,
            $liveOffset,
            $liveBytes.Length - $liveOffset)
        if ($liveRead -le 0) {
            throw 'Unexpected end of committed profile'
        }
        $liveOffset += $liveRead
    }
    $committedProfileMatches = $liveBytes.Length -eq $newBytes.Length
    if ($committedProfileMatches) {
        for ($index = 0; $index -lt $newBytes.Length; $index++) {
            if ($liveBytes[$index] -ne $newBytes[$index]) {
                $committedProfileMatches = $false
                break
            }
        }
    }

    $failureStage = 'verify-replaced-snapshot'
    $replacedBytes = [System.IO.File]::ReadAllBytes($replaceBackup)
    $replacedSnapshotMatches = $replacedBytes.Length -eq $originalBytes.Length
    if ($replacedSnapshotMatches) {
        for ($index = 0; $index -lt $originalBytes.Length; $index++) {
            if ($replacedBytes[$index] -ne $originalBytes[$index]) {
                $replacedSnapshotMatches = $false
                break
            }
        }
    }
    if (-not $replacedSnapshotMatches -or -not $committedProfileMatches) {
        $failureStage = 'restore-concurrent-write'
        $repairCandidateBackup = $backup + '.repair-candidate'
        $liveStream.Unlock(0, [long]::MaxValue)
        $liveLocked = $false
        $stream.Dispose()
        $stream = $null
        [System.IO.File]::Replace(
            $replaceBackup,
            $path,
            $repairCandidateBackup,
            $true)
        $committed = $false
        StopRepair 12
    }
    $stream.Dispose()
    $stream = $null
    $liveStream.Unlock(0, [long]::MaxValue)
    $liveLocked = $false
    $liveStream.Dispose()
    $liveStream = $null
    [System.IO.File]::Delete($replaceBackup)
    $outcome = 10
}
catch {
    $caught = $_.Exception
    $requestedOutcome = $null
    if ($caught.Data.Contains('ITProfileOutcome')) {
        $requestedOutcome = [int]$caught.Data['ITProfileOutcome']
    }
    if ($committed -and
        $null -ne $replaceBackup -and
        [System.IO.File]::Exists($replaceBackup)) {
        try {
            if ($null -ne $liveStream) {
                if ($liveLocked) {
                    $liveStream.Unlock(0, [long]::MaxValue)
                    $liveLocked = $false
                }
            }
            if ($null -ne $stream) {
                $stream.Dispose()
                $stream = $null
            }
            $failureStage = 'recover-failed-commit'
            $failedCandidateBackup = $replaceBackup + '.failed-candidate'
            [System.IO.File]::Replace(
                $replaceBackup,
                $path,
                $failedCandidateBackup,
                $true)
            if ($null -ne $liveStream) {
                $liveStream.Dispose()
                $liveStream = $null
            }
            $committed = $false
        }
        catch {
            $caught = $_.Exception
            $failureStage = 'recovery-failed'
            $requestedOutcome = $null
            $recoveryFailedAfterCommit = $committedProfileMatches
        }
    }
    if ($recoveryFailedAfterCommit) {
        $outcome = 14
    }
    elseif ($null -ne $requestedOutcome) {
        $outcome = $requestedOutcome
    }
    else {
        $outcome = 13
        $failure = $caught
        if ($null -ne $failure.InnerException) {
            $failure = $failure.InnerException
        }
        $failureType = $failure.GetType().FullName
        $failureHResult = $failure.HResult
    }
}
finally {
    if ($null -ne $stream) {
        if ($locked) {
            try { $stream.Unlock(0, [long]::MaxValue) } catch {}
        }
        $stream.Dispose()
    }
    if ($null -ne $liveStream) {
        if ($liveLocked) {
            try { $liveStream.Unlock(0, [long]::MaxValue) } catch {}
        }
        $liveStream.Dispose()
    }
    if ($null -ne $tempPath) {
        try { [System.IO.File]::Delete($tempPath) } catch {}
    }
}

Complete $outcome
)PS";

        inline std::wstring NormalizedPath(const std::filesystem::path& path)
        {
            std::error_code ec;
            auto normalized = std::filesystem::canonical(path, ec).wstring();
            if (ec)
            {
                return {};
            }
            std::replace(normalized.begin(), normalized.end(), L'/', L'\\');
            std::transform(normalized.begin(), normalized.end(), normalized.begin(), [](const wchar_t value) {
                return static_cast<wchar_t>(towlower(value));
            });
            return normalized;
        }

        inline bool IsPathWithin(std::wstring_view path, std::wstring_view root)
        {
            return path.size() > root.size() &&
                   path.substr(0, root.size()) == root &&
                   path[root.size()] == L'\\';
        }

        inline bool ExecutableMatchesTarget(Target target, const std::filesystem::path& executable)
        {
            const auto normalized = NormalizedPath(executable);
            if (normalized.empty())
            {
                return false;
            }

            if (target == Target::WindowsPowerShell)
            {
                wchar_t system32[MAX_PATH]{};
                const auto length = GetSystemDirectoryW(system32, MAX_PATH);
                if (length == 0 || length >= MAX_PATH)
                {
                    return false;
                }
                const auto expected = NormalizedPath(
                    std::filesystem::path{ std::wstring_view{ system32, length } } /
                    L"WindowsPowerShell" / L"v1.0" / L"powershell.exe");
                return !expected.empty() && normalized == expected;
            }

            if (target != Target::Pwsh ||
                std::filesystem::path{ normalized }.filename() != L"pwsh.exe")
            {
                return false;
            }

            wil::unique_cotaskmem_string programFiles;
            if (FAILED(SHGetKnownFolderPath(FOLDERID_ProgramFiles, 0, nullptr, &programFiles)) ||
                !programFiles)
            {
                return false;
            }
            const auto powerShellRoot = NormalizedPath(
                std::filesystem::path{ programFiles.get() } / L"PowerShell");
            if (!powerShellRoot.empty() && IsPathWithin(normalized, powerShellRoot))
            {
                return true;
            }

            const auto windowsApps = NormalizedPath(
                std::filesystem::path{ programFiles.get() } / L"WindowsApps");
            if (windowsApps.empty() || !IsPathWithin(normalized, windowsApps))
            {
                return false;
            }
            const auto relative = normalized.substr(windowsApps.size() + 1);
            return relative.starts_with(L"microsoft.powershell_");
        }

        inline ProfileRepairResult RunRepairProcess(Target target,
                                                    const std::filesystem::path& executable,
                                                    const std::filesystem::path& profilePath) noexcept
        {
            if (!ExecutableMatchesTarget(target, executable))
            {
                return { ProfileRepairOutcome::Unsupported, L"PowerShell host identity did not match repair target" };
            }

            try
            {
                SECURITY_ATTRIBUTES sa{};
                sa.nLength = sizeof(sa);
                sa.bInheritHandle = TRUE;

                HANDLE rawStdoutRead = nullptr;
                HANDLE rawStdoutWrite = nullptr;
                HANDLE rawStdinRead = nullptr;
                HANDLE rawStdinWrite = nullptr;
                if (!CreatePipe(&rawStdoutRead, &rawStdoutWrite, &sa, 0) ||
                    !CreatePipe(&rawStdinRead, &rawStdinWrite, &sa, 0))
                {
                    if (rawStdoutRead) CloseHandle(rawStdoutRead);
                    if (rawStdoutWrite) CloseHandle(rawStdoutWrite);
                    if (rawStdinRead) CloseHandle(rawStdinRead);
                    if (rawStdinWrite) CloseHandle(rawStdinWrite);
                    return { ProfileRepairOutcome::Failed, L"Failed to create PowerShell repair pipes" };
                }
                wil::unique_handle stdoutRead{ rawStdoutRead };
                wil::unique_handle stdoutWrite{ rawStdoutWrite };
                wil::unique_handle stdinRead{ rawStdinRead };
                wil::unique_handle stdinWrite{ rawStdinWrite };
                SetHandleInformation(stdoutRead.get(), HANDLE_FLAG_INHERIT, 0);
                SetHandleInformation(stdinWrite.get(), HANDLE_FLAG_INHERIT, 0);

                STARTUPINFOW si{};
                si.cb = sizeof(si);
                si.dwFlags = STARTF_USESTDHANDLES | STARTF_USESHOWWINDOW;
                si.wShowWindow = SW_HIDE;
                si.hStdOutput = stdoutWrite.get();
                si.hStdError = stdoutWrite.get();
                si.hStdInput = stdinRead.get();

                std::wstring commandLine = L"\"" + executable.wstring() +
                    L"\" -NoLogo -NoProfile -NonInteractive -Command -";

                PROCESS_INFORMATION pi{};
                if (!CreateProcessW(executable.c_str(),
                                    commandLine.data(),
                                    nullptr,
                                    nullptr,
                                    TRUE,
                                    CREATE_NO_WINDOW,
                                    nullptr,
                                    nullptr,
                                    &si,
                                    &pi))
                {
                    return { ProfileRepairOutcome::Failed, L"Failed to start PowerShell profile repair" };
                }
                wil::unique_handle process{ pi.hProcess };
                wil::unique_handle thread{ pi.hThread };
                stdoutWrite.reset();
                stdinRead.reset();

                const auto encodedPath = Base64EncodeUtf16(profilePath.wstring());
                const auto pathAssignment = L"$path64 = '" + encodedPath + L"'\r\n";
                const auto stdinPayload =
                    til::u16u8(pathAssignment) +
                    til::u16u8(std::wstring{ RepairScript });
                DWORD written = 0;
                const bool wrotePath = WriteFile(
                    stdinWrite.get(),
                    stdinPayload.data(),
                    static_cast<DWORD>(stdinPayload.size()),
                    &written,
                    nullptr) &&
                    written == stdinPayload.size();
                stdinWrite.reset();
                if (!wrotePath)
                {
                    TerminateProcess(process.get(), 1);
                    WaitForSingleObject(process.get(), 1000);
                    return { ProfileRepairOutcome::Failed, L"Failed to send profile path to PowerShell repair" };
                }

                constexpr DWORD timeoutMs = 30000;
                if (WaitForSingleObject(process.get(), timeoutMs) != WAIT_OBJECT_0)
                {
                    TerminateProcess(process.get(), 1);
                    WaitForSingleObject(process.get(), 1000);
                    return { ProfileRepairOutcome::Failed, L"PowerShell profile repair timed out" };
                }

                DWORD exitCode = 0;
                if (!GetExitCodeProcess(process.get(), &exitCode))
                {
                    return { ProfileRepairOutcome::Failed, L"Failed to read PowerShell profile repair result" };
                }
                std::string childOutput;
                char buffer[512];
                DWORD bytesRead = 0;
                while (childOutput.size() < 4096 &&
                       ReadFile(stdoutRead.get(), buffer, sizeof(buffer), &bytesRead, nullptr) &&
                       bytesRead > 0)
                {
                    childOutput.append(buffer, bytesRead);
                }
                switch (exitCode)
                {
                case 10:
                    return { ProfileRepairOutcome::Repaired, {} };
                case 11:
                    return { ProfileRepairOutcome::NotApplicable, {} };
                case 12:
                    return { ProfileRepairOutcome::Unsupported, L"PowerShell profile shape is not safe for automatic repair" };
                case 14:
                    return {
                        ProfileRepairOutcome::RepairedWithRecoveryBackup,
                        L"PowerShell profile was repaired, but a concurrent version remains in the recovery backup"
                    };
                default:
                {
                    std::wstring diagnostic;
                    diagnostic.reserve(childOutput.size());
                    for (const auto value : childOutput)
                    {
                        if (value >= 0x20 && value <= 0x7e)
                        {
                            diagnostic.push_back(static_cast<wchar_t>(value));
                        }
                    }
                    return {
                        ProfileRepairOutcome::Failed,
                        diagnostic.empty() ?
                            L"PowerShell profile repair failed" :
                            L"PowerShell profile repair failed (" + diagnostic + L")"
                    };
                }
                }
            }
            catch (...)
            {
                return { ProfileRepairOutcome::Failed, L"PowerShell profile repair failed unexpectedly" };
            }
        }
    }

    inline bool IsTrustedPowerShellHost(
        Target target,
        const std::filesystem::path& executable)
    {
        return repair_details::ExecutableMatchesTarget(target, executable);
    }

    inline ProfileRepairResult RepairProfileAfterOhMyPosh(
        Target target,
        const std::filesystem::path& executable,
        const std::filesystem::path& profilePath)
    {
        if (target != Target::Pwsh && target != Target::WindowsPowerShell)
        {
            return { ProfileRepairOutcome::Unsupported, L"Profile repair only supports PowerShell" };
        }
        if (profilePath.empty())
        {
            return { ProfileRepairOutcome::Failed, L"PowerShell profile path is empty" };
        }

        std::lock_guard<std::mutex> profileGuard{
            ::Microsoft::Terminal::ShellIntegration::details::ProfileFileMutex()
        };
        return repair_details::RunRepairProcess(target, executable, profilePath);
    }

    inline ProfileRepairResult RepairProfileAfterOhMyPosh(
        Target target,
        const std::filesystem::path& executable)
    {
        const auto profilePath = DiscoverProfilePath(target);
        if (profilePath.empty())
        {
            return { ProfileRepairOutcome::Failed, L"Could not discover PowerShell profile path" };
        }
        return RepairProfileAfterOhMyPosh(target, executable, profilePath);
    }
}
