// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// PowerShellProfileAnalyzer.cpp
//
// Runs the matching PowerShell host without loading a profile. The child parses
// a framed in-memory snapshot using PowerShell's public parser API; it never
// dot-sources or otherwise evaluates profile content.

#include "pch.h"
#include "PowerShellProfileAnalyzer.h"

#include <array>
#include <algorithm>
#include <atomic>
#include <chrono>
#include <cstring>
#include <limits>
#include <thread>
#include <vector>

#include "../inc/PowerShellShellIntegration.h"

using namespace Microsoft::Terminal::ShellIntegration;
using namespace Microsoft::Terminal::ShellIntegration::Health;

namespace
{
    constexpr size_t MaximumProfileBytes{ 1024 * 1024 };
    constexpr size_t MaximumChildOutputBytes{ 16 * 1024 };
    constexpr DWORD AnalyzerTimeoutMilliseconds{ 15 * 1000 };

    enum class HostKind
    {
        Pwsh,
        WindowsPowerShell,
        Invalid,
    };

    struct ChildResult
    {
        std::string standardOutput;
        std::string standardError;
        std::atomic_bool outputExceeded{ false };
    };

    bool _EqualsInsensitive(const std::wstring_view left, const std::wstring_view right) noexcept
    {
        if (left.size() > static_cast<size_t>(std::numeric_limits<int>::max()) ||
            right.size() > static_cast<size_t>(std::numeric_limits<int>::max()))
        {
            return false;
        }
        return left.size() == right.size() &&
               CompareStringOrdinal(left.data(),
                                    gsl::narrow_cast<int>(left.size()),
                                    right.data(),
                                    gsl::narrow_cast<int>(right.size()),
                                    TRUE) == CSTR_EQUAL;
    }

    bool _IsAbsoluteWindowsPath(std::wstring_view path) noexcept
    {
        if (path.starts_with(L"\\\\?\\UNC\\"))
        {
            // An extended UNC path has no leading separators after its UNC\
            // prefix, so validate its server/share portion directly.
            path.remove_prefix(8);
            const auto serverEnd = path.find(L'\\');
            return serverEnd != std::wstring_view::npos && serverEnd != 0 &&
                   serverEnd + 1 < path.size() &&
                   path.find(L'\\', serverEnd + 1) != serverEnd + 1;
        }
        if (path.starts_with(L"\\\\?\\"))
        {
            path.remove_prefix(4);
        }
        if (path.size() >= 3 &&
            ((path[0] >= L'A' && path[0] <= L'Z') || (path[0] >= L'a' && path[0] <= L'z')) &&
            path[1] == L':' && (path[2] == L'\\' || path[2] == L'/'))
        {
            return true;
        }
        if (path.size() < 5 || path[0] != L'\\' || path[1] != L'\\')
        {
            return false;
        }
        const auto serverEnd = path.find(L'\\', 2);
        return serverEnd != std::wstring_view::npos && serverEnd + 1 < path.size() &&
               path.find(L'\\', serverEnd + 1) != serverEnd + 1;
    }

    std::wstring_view _LeafName(const std::wstring_view path) noexcept
    {
        const auto separator = path.find_last_of(L"\\/");
        return separator == std::wstring_view::npos ? path : path.substr(separator + 1);
    }

    HostKind _GetHostKind(const std::wstring_view hostPath) noexcept
    {
        if (hostPath.empty() || !_IsAbsoluteWindowsPath(hostPath))
        {
            return HostKind::Invalid;
        }
        if (_EqualsInsensitive(_LeafName(hostPath), L"pwsh.exe"))
        {
            return HostKind::Pwsh;
        }
        if (_EqualsInsensitive(_LeafName(hostPath), L"powershell.exe"))
        {
            return HostKind::WindowsPowerShell;
        }
        return HostKind::Invalid;
    }

    std::string _Base64Encode(const std::string_view value)
    {
        constexpr char Alphabet[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        std::string encoded;
        encoded.reserve(((value.size() + 2) / 3) * 4);
        for (size_t index = 0; index < value.size(); index += 3)
        {
            const auto first = static_cast<unsigned char>(value[index]);
            const auto second = index + 1 < value.size() ? static_cast<unsigned char>(value[index + 1]) : 0;
            const auto third = index + 2 < value.size() ? static_cast<unsigned char>(value[index + 2]) : 0;
            encoded.push_back(Alphabet[first >> 2]);
            encoded.push_back(Alphabet[((first & 0x03) << 4) | (second >> 4)]);
            encoded.push_back(index + 1 < value.size() ? Alphabet[((second & 0x0f) << 2) | (third >> 6)] : '=');
            encoded.push_back(index + 2 < value.size() ? Alphabet[third & 0x3f] : '=');
        }
        return encoded;
    }

    std::wstring _EncodePowerShellCommand(const std::string_view script)
    {
        std::wstring utf16;
        utf16.reserve(script.size());
        for (const auto ch : script)
        {
            utf16.push_back(static_cast<unsigned char>(ch));
        }
        return til::u8u16(_Base64Encode(std::string_view{
            reinterpret_cast<const char*>(utf16.data()),
            utf16.size() * sizeof(wchar_t) }));
    }

    std::string _BuildParserScript(const HostKind host)
    {
        const auto subdir = host == HostKind::Pwsh ? L"PowerShell" : L"WindowsPowerShell";
        const auto expectedLf = _Base64Encode(Powershell::BuildBlock(subdir, "\n"));
        const auto expectedCrLf = _Base64Encode(Powershell::BuildBlock(subdir, "\r\n"));
        const auto expectedScriptFileName = til::u16u8(Powershell::ScriptFileName());
        const std::string noBomEncoding = host == HostKind::WindowsPowerShell ?
                                               "[Text.Encoding]::Default" :
                                               "(New-Object Text.UTF8Encoding($false,$true))";

        // The snapshot frame is: ASCII "ITPA", an unsigned little-endian
        // payload length, then exactly that many unmodified profile bytes.
        // Output uses only ASCII and contains no profile-derived strings.
        return R"($ErrorActionPreference='Stop'
$ProgressPreference='SilentlyContinue'
function Read-Exactly([System.IO.Stream]$Stream,[int]$Count) {
    $buffer=New-Object byte[] $Count
    $offset=0
    while($offset -lt $Count) {
        $read=$Stream.Read($buffer,$offset,$Count-$offset)
        if($read -le 0) { throw 'truncated input' }
        $offset+=$read
    }
    return ,$buffer
}
function Emit([string]$Status,[string]$Reason,[UInt64]$Start,[UInt64]$End) {
    $line='ITPA1|'+$Status+'|'+$Reason+'|'+$Start+'|'+$End+"`n"
    $out=[Console]::OpenStandardOutput()
    $raw=[Text.Encoding]::ASCII.GetBytes($line)
    $out.Write($raw,0,$raw.Length)
    $out.Flush()
}
try {
    $stream=[Console]::OpenStandardInput()
    $header=Read-Exactly $stream 8
    if([Text.Encoding]::ASCII.GetString($header,0,4) -ne 'ITPA') { throw 'bad frame' }
    $length=[BitConverter]::ToUInt32($header,4)
    if($length -gt 1048576) { throw 'oversized frame' }
    $bytes=Read-Exactly $stream ([int]$length)
    $offset=0
    if($bytes.Length -ge 3 -and $bytes[0] -eq 239 -and $bytes[1] -eq 187 -and $bytes[2] -eq 191) {
        $encoding=New-Object Text.UTF8Encoding($false,$true); $offset=3
    } elseif($bytes.Length -ge 2 -and $bytes[0] -eq 255 -and $bytes[1] -eq 254) {
        $encoding=[Text.Encoding]::Unicode; $offset=2
    } elseif($bytes.Length -ge 2 -and $bytes[0] -eq 254 -and $bytes[1] -eq 255) {
        $encoding=[Text.Encoding]::BigEndianUnicode; $offset=2
    } else {
        $encoding=)" + noBomEncoding + R"(
    }
    $text=$encoding.GetString($bytes,$offset,$bytes.Length-$offset)
    [System.Management.Automation.Language.Token[]]$tokens=$null
    [System.Management.Automation.Language.ParseError[]]$errors=$null
    $ast=[System.Management.Automation.Language.Parser]::ParseInput($text,[ref]$tokens,[ref]$errors)
    if($errors -and $errors.Count -ne 0) { Emit 'Indeterminate' 'ParseFailed' 0 0; exit 0 }
    $open='# >>> intelligent-terminal shell-integration >>>'
    $close='# <<< intelligent-terminal shell-integration <<<'
    $opens=@($tokens | Where-Object { $_.Kind -eq [System.Management.Automation.Language.TokenKind]::Comment -and $_.Text.Trim() -ceq $open })
    $closes=@($tokens | Where-Object { $_.Kind -eq [System.Management.Automation.Language.TokenKind]::Comment -and $_.Text.Trim() -ceq $close })
    if($opens.Count -eq 0 -and $closes.Count -eq 0) { Emit 'NotInstalled' 'MissingBlock' 0 0; exit 0 }
    if($opens.Count -ne 1 -or $closes.Count -ne 1 -or $opens[0].Extent.StartOffset -ge $closes[0].Extent.StartOffset) {
        Emit 'Indeterminate' 'MalformedBlock' 0 0; exit 0
    }
    $start=[int]$opens[0].Extent.StartOffset
    $end=[int]$closes[0].Extent.EndOffset
    if(($start -ne 0 -and $text[$start-1] -ne "`n") -or
       ($closes[0].Extent.StartOffset -ne 0 -and $text[$closes[0].Extent.StartOffset-1] -ne "`n")) {
        Emit 'Indeterminate' 'MalformedBlock' 0 0; exit 0
    }
    $containingRootStatement=@($ast.EndBlock.Statements | Where-Object {
        $_.Extent.StartOffset -le $start -and $_.Extent.EndOffset -ge $end
    })
    if($containingRootStatement.Count -ne 0) {
        Emit 'Indeterminate' 'MalformedBlock' 0 0; exit 0
    }
    $block=$text.Substring($start,$end-$start)
    $expectedLf=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String(')" + expectedLf + R"('))
    $expectedCrLf=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String(')" + expectedCrLf + R"('))
    $versionPattern='shell-integration_v[1-9][0-9]*\.ps1'
    $versionMatches=@([regex]::Matches($block,$versionPattern,[Text.RegularExpressions.RegexOptions]::CultureInvariant))
    if($versionMatches.Count -ne 1) { Emit 'Indeterminate' 'MalformedBlock' 0 0; exit 0 }
    $normalizedBlock=[regex]::Replace($block,$versionPattern,')" + expectedScriptFileName + R"(',[Text.RegularExpressions.RegexOptions]::CultureInvariant)
    if($normalizedBlock -cne $expectedLf -and $normalizedBlock -cne $expectedCrLf) { Emit 'Indeterminate' 'MalformedBlock' 0 0; exit 0 }
    $tail=$text.Substring($end)
    [System.Management.Automation.Language.Token[]]$tailTokens=$null
    [System.Management.Automation.Language.ParseError[]]$tailErrors=$null
    $tailAst=[System.Management.Automation.Language.Parser]::ParseInput($tail,[ref]$tailTokens,[ref]$tailErrors)
    if($tailErrors -and $tailErrors.Count -ne 0) { Emit 'Indeterminate' 'ParseFailed' 0 0; exit 0 }
    if(@($tailAst.EndBlock.Statements).Count -ne 0 -or
       ($null -ne $tailAst.EndBlock.Traps -and $tailAst.EndBlock.Traps.Count -ne 0)) {
        $before=$encoding.GetByteCount($text.Substring(0,$start))+$offset
        $through=$encoding.GetByteCount($text.Substring(0,$end))+$offset
        Emit 'BlockNotLast' 'None' ([UInt64]$before) ([UInt64]$through); exit 0
    }
    $before=$encoding.GetByteCount($text.Substring(0,$start))+$offset
    $through=$encoding.GetByteCount($text.Substring(0,$end))+$offset
    Emit 'Healthy' 'None' ([UInt64]$before) ([UInt64]$through)
} catch {
    Emit 'Indeterminate' 'ParseFailed' 0 0
})";
    }

    void _DrainPipe(HANDLE pipe, std::string& destination, std::atomic_bool& exceeded) noexcept
    {
        try
        {
            std::array<char, 4096> buffer;
            DWORD read{};
            while (ReadFile(pipe, buffer.data(), gsl::narrow_cast<DWORD>(buffer.size()), &read, nullptr) && read != 0)
            {
                const auto available = MaximumChildOutputBytes - (std::min)(destination.size(), MaximumChildOutputBytes);
                const auto retained = (std::min)(available, static_cast<size_t>(read));
                destination.append(buffer.data(), retained);
                if (retained != read)
                {
                    exceeded.store(true, std::memory_order_relaxed);
                }
            }
        }
        catch (...)
        {
            exceeded.store(true, std::memory_order_relaxed);
        }
    }

    void _WriteSnapshot(HANDLE pipe, const std::string_view snapshot) noexcept
    {
        std::array<unsigned char, 8> header{ 'I', 'T', 'P', 'A' };
        const auto size = gsl::narrow_cast<uint32_t>(snapshot.size());
        memcpy(header.data() + 4, &size, sizeof(size));

        const auto writeAll = [&](const void* source, size_t count) {
            auto bytes = static_cast<const char*>(source);
            while (count != 0)
            {
                DWORD written{};
                const auto request = static_cast<DWORD>((std::min)(count, static_cast<size_t>(64 * 1024)));
                if (!WriteFile(pipe, bytes, request, &written, nullptr) || written == 0)
                {
                    return;
                }
                bytes += written;
                count -= written;
            }
        };
        writeAll(header.data(), header.size());
        writeAll(snapshot.data(), snapshot.size());
    }

    bool _SameProcessImage(const HANDLE process, const std::wstring_view requested) noexcept
    {
        std::array<wchar_t, 32768> image{};
        DWORD length = gsl::narrow_cast<DWORD>(image.size());
        if (!QueryFullProcessImageNameW(process, 0, image.data(), &length))
        {
            return false;
        }
        return _EqualsInsensitive(requested, std::wstring_view{ image.data(), length });
    }

    AnalysisResult _ParseChildResult(const ChildResult& child, const size_t snapshotSize) noexcept
    {
        if (child.outputExceeded.load(std::memory_order_relaxed))
        {
            return { Status::Indeterminate, Reason::ParseFailed };
        }

        // stderr is drained and bounded for liveness, but is not protocol
        // input. Hosts can emit non-fatal diagnostics there; accepting only a
        // complete, ASCII-only stdout frame avoids surfacing any such content.
        constexpr std::string_view prefix{ "ITPA1|" };
        if (!child.standardOutput.starts_with(prefix) ||
            child.standardOutput.find('\n') == std::string::npos)
        {
            return { Status::Indeterminate, Reason::ParseFailed };
        }

        const auto newline = child.standardOutput.find('\n');
        if (newline + 1 != child.standardOutput.size())
        {
            return { Status::Indeterminate, Reason::ParseFailed };
        }

        std::array<std::string_view, 4> fields;
        std::string_view remainder = std::string_view{ child.standardOutput }.substr(prefix.size(), newline - prefix.size());
        for (auto& field : fields)
        {
            const auto separator = remainder.find('|');
            field = remainder.substr(0, separator);
            if (separator == std::string_view::npos)
            {
                if (&field != &fields.back())
                {
                    return { Status::Indeterminate, Reason::ParseFailed };
                }
                remainder = {};
            }
            else
            {
                remainder.remove_prefix(separator + 1);
            }
        }
        if (!remainder.empty())
        {
            return { Status::Indeterminate, Reason::ParseFailed };
        }

        const auto number = [](const std::string_view value, size_t& result) noexcept {
            if (value.empty())
            {
                return false;
            }
            uint64_t converted{};
            for (const auto ch : value)
            {
                if (ch < '0' || ch > '9' || converted > (std::numeric_limits<uint64_t>::max() - (ch - '0')) / 10)
                {
                    return false;
                }
                converted = converted * 10 + (ch - '0');
            }
            if (converted > std::numeric_limits<size_t>::max())
            {
                return false;
            }
            result = static_cast<size_t>(converted);
            return true;
        };

        Status status{};
        Reason reason{};
        if (fields[0] == "Healthy" && fields[1] == "None")
        {
            status = Status::Healthy;
            reason = Reason::None;
        }
        else if (fields[0] == "BlockNotLast" && fields[1] == "None")
        {
            status = Status::BlockNotLast;
            reason = Reason::None;
        }
        else if (fields[0] == "NotInstalled" && fields[1] == "MissingBlock")
        {
            return { Status::NotInstalled, Reason::MissingBlock };
        }
        else if (fields[0] == "Indeterminate" && fields[1] == "MalformedBlock")
        {
            return { Status::Indeterminate, Reason::MalformedBlock };
        }
        else if (fields[0] == "Indeterminate" && fields[1] == "ParseFailed")
        {
            return { Status::Indeterminate, Reason::ParseFailed };
        }
        else
        {
            return { Status::Indeterminate, Reason::ParseFailed };
        }

        size_t start{};
        size_t end{};
        if (!number(fields[2], start) || !number(fields[3], end) || end < start || end > snapshotSize)
        {
            return { Status::Indeterminate, Reason::ParseFailed };
        }
        return { status, reason, start, end };
    }
}

namespace Microsoft::Terminal::ShellIntegration::Powershell::ProfileAnalyzer
{
    AnalysisResult Analyze(const std::wstring_view hostPath,
                           const std::string_view profileBytes) noexcept
    {
        try
        {
            const auto host = _GetHostKind(hostPath);
            const auto fileAttributes = GetFileAttributesW(std::wstring{ hostPath }.c_str());
            if (host == HostKind::Invalid ||
                fileAttributes == INVALID_FILE_ATTRIBUTES ||
                (fileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0)
            {
                return { Status::Indeterminate, Reason::UnsupportedHost };
            }
            if (profileBytes.size() > MaximumProfileBytes)
            {
                return { Status::Indeterminate, Reason::FileTooLarge };
            }

            SECURITY_ATTRIBUTES inheritable{};
            inheritable.nLength = sizeof(inheritable);
            inheritable.bInheritHandle = TRUE;

            wil::unique_handle childInput;
            wil::unique_handle parentInput;
            wil::unique_handle parentOutput;
            wil::unique_handle childOutput;
            wil::unique_handle parentError;
            wil::unique_handle childError;
            if (!CreatePipe(childInput.addressof(), parentInput.addressof(), &inheritable, 0) ||
                !CreatePipe(parentOutput.addressof(), childOutput.addressof(), &inheritable, 0) ||
                !CreatePipe(parentError.addressof(), childError.addressof(), &inheritable, 0) ||
                !SetHandleInformation(parentInput.get(), HANDLE_FLAG_INHERIT, 0) ||
                !SetHandleInformation(parentOutput.get(), HANDLE_FLAG_INHERIT, 0) ||
                !SetHandleInformation(parentError.get(), HANDLE_FLAG_INHERIT, 0))
            {
                return { Status::Indeterminate, Reason::UnsupportedInvocation };
            }

            std::array<HANDLE, 3> inheritedHandles{
                childInput.get(),
                childOutput.get(),
                childError.get(),
            };
            SIZE_T attributeBytes{};
            InitializeProcThreadAttributeList(nullptr, 1, 0, &attributeBytes);
            auto attributes = static_cast<PPROC_THREAD_ATTRIBUTE_LIST>(HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, attributeBytes));
            if (!attributes)
            {
                return { Status::Indeterminate, Reason::UnsupportedInvocation };
            }
            const auto freeAttributes = wil::scope_exit([&] {
                HeapFree(GetProcessHeap(), 0, attributes);
            });
            if (!InitializeProcThreadAttributeList(attributes, 1, 0, &attributeBytes))
            {
                return { Status::Indeterminate, Reason::UnsupportedInvocation };
            }
            const auto deleteAttributes = wil::scope_exit([&] {
                DeleteProcThreadAttributeList(attributes);
            });
            if (!UpdateProcThreadAttribute(attributes,
                                            0,
                                            PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
                                            inheritedHandles.data(),
                                            gsl::narrow_cast<SIZE_T>(inheritedHandles.size() * sizeof(HANDLE)),
                                            nullptr,
                                            nullptr))
            {
                return { Status::Indeterminate, Reason::UnsupportedInvocation };
            }

            const auto script = _BuildParserScript(host);
            const auto encodedScript = _EncodePowerShellCommand(script);
            std::wstring commandLine;
            commandLine.reserve(hostPath.size() + encodedScript.size() + 96);
            commandLine.append(L"\"");
            commandLine.append(hostPath);
            commandLine.append(L"\" -NoLogo -NoProfile -NonInteractive -EncodedCommand ");
            commandLine.append(encodedScript);

            STARTUPINFOEXW startup{};
            startup.StartupInfo.cb = sizeof(startup);
            startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES | STARTF_USESHOWWINDOW;
            startup.StartupInfo.wShowWindow = SW_HIDE;
            startup.StartupInfo.hStdInput = childInput.get();
            startup.StartupInfo.hStdOutput = childOutput.get();
            startup.StartupInfo.hStdError = childError.get();
            startup.lpAttributeList = attributes;

            PROCESS_INFORMATION processInformation{};
            if (!CreateProcessW(std::wstring{ hostPath }.c_str(),
                                commandLine.data(),
                                nullptr,
                                nullptr,
                                TRUE,
                                CREATE_NO_WINDOW | CREATE_SUSPENDED | EXTENDED_STARTUPINFO_PRESENT,
                                nullptr,
                                nullptr,
                                &startup.StartupInfo,
                                &processInformation))
            {
                return { Status::Indeterminate, Reason::UnsupportedHost };
            }
            wil::unique_handle process{ processInformation.hProcess };
            wil::unique_handle thread{ processInformation.hThread };

            wil::unique_handle job{ CreateJobObjectW(nullptr, nullptr) };
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits{};
            if (!job)
            {
                TerminateProcess(process.get(), 1);
                return { Status::Indeterminate, Reason::UnsupportedInvocation };
            }
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if (!SetInformationJobObject(job.get(), JobObjectExtendedLimitInformation, &limits, sizeof(limits)) ||
                !AssignProcessToJobObject(job.get(), process.get()) ||
                !_SameProcessImage(process.get(), hostPath))
            {
                TerminateProcess(process.get(), 1);
                return { Status::Indeterminate, Reason::ShellIdentityUnavailable };
            }
            if (ResumeThread(thread.get()) == static_cast<DWORD>(-1))
            {
                TerminateProcess(process.get(), 1);
                return { Status::Indeterminate, Reason::UnsupportedInvocation };
            }

            childInput.reset();
            childOutput.reset();
            childError.reset();
            ChildResult child;
            std::thread inputWriter;
            std::thread outputReader;
            std::thread errorReader;
            try
            {
                inputWriter = std::thread{ _WriteSnapshot, parentInput.get(), profileBytes };
                outputReader = std::thread{ _DrainPipe, parentOutput.get(), std::ref(child.standardOutput), std::ref(child.outputExceeded) };
                errorReader = std::thread{ _DrainPipe, parentError.get(), std::ref(child.standardError), std::ref(child.outputExceeded) };
            }
            catch (...)
            {
                job.reset();
                if (inputWriter.joinable())
                {
                    CancelSynchronousIo(inputWriter.native_handle());
                    inputWriter.join();
                }
                if (outputReader.joinable())
                {
                    outputReader.join();
                }
                if (errorReader.joinable())
                {
                    errorReader.join();
                }
                return { Status::Indeterminate, Reason::UnsupportedInvocation };
            }

            DWORD waitResult{};
            const auto deadline = std::chrono::steady_clock::now() + std::chrono::milliseconds{ AnalyzerTimeoutMilliseconds };
            do
            {
                waitResult = WaitForSingleObject(process.get(), 50);
                if (child.outputExceeded.load(std::memory_order_relaxed))
                {
                    waitResult = WAIT_TIMEOUT;
                    break;
                }
            } while (waitResult == WAIT_TIMEOUT && std::chrono::steady_clock::now() < deadline);

            bool childExitFailed = false;
            if (waitResult == WAIT_OBJECT_0)
            {
                DWORD exitCode{};
                childExitFailed = !GetExitCodeProcess(process.get(), &exitCode) || exitCode != 0;
            }

            // Closing the job terminates the full child process tree and closes
            // any inherited pipe ends before joining the blocking drain threads.
            // Cancel the writer as an additional guarantee if it was blocked
            // while the child stopped consuming stdin.
            job.reset();
            if (inputWriter.joinable())
            {
                CancelSynchronousIo(inputWriter.native_handle());
                inputWriter.join();
            }
            parentInput.reset();
            if (outputReader.joinable())
            {
                outputReader.join();
            }
            if (errorReader.joinable())
            {
                errorReader.join();
            }

            if (child.outputExceeded.load(std::memory_order_relaxed))
            {
                return { Status::Indeterminate, Reason::ParseFailed };
            }
            if (waitResult == WAIT_TIMEOUT)
            {
                return { Status::Indeterminate, Reason::TimedOut };
            }
            if (waitResult != WAIT_OBJECT_0)
            {
                return { Status::Indeterminate, Reason::UnsupportedInvocation };
            }
            if (childExitFailed)
            {
                return { Status::Indeterminate, Reason::UnsupportedInvocation };
            }
            return _ParseChildResult(child, profileBytes.size());
        }
        catch (...)
        {
            return { Status::Indeterminate, Reason::UnsupportedInvocation };
        }
    }
}
