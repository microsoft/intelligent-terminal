// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "GitProcessRunner.h"

#include <array>
#include <map>

namespace Microsoft::Terminal::RepoAwareness
{
    namespace
    {
        struct CaseInsensitiveLess
        {
            bool operator()(const std::wstring& lhs, const std::wstring& rhs) const noexcept
            {
                return _wcsicmp(lhs.c_str(), rhs.c_str()) < 0;
            }
        };

        std::wstring _quoteArgument(const std::wstring_view argument)
        {
            if (!argument.empty() && argument.find_first_of(L" \t\n\v\"") == std::wstring_view::npos)
            {
                return std::wstring{ argument };
            }

            std::wstring result{ L'"' };
            size_t backslashes = 0;
            for (const auto character : argument)
            {
                if (character == L'\\')
                {
                    ++backslashes;
                    continue;
                }

                if (character == L'"')
                {
                    result.append(backslashes * 2 + 1, L'\\');
                    result.push_back(L'"');
                }
                else
                {
                    result.append(backslashes, L'\\');
                    result.push_back(character);
                }
                backslashes = 0;
            }

            result.append(backslashes * 2, L'\\');
            result.push_back(L'"');
            return result;
        }

        std::wstring _buildCommandLine(const std::filesystem::path& executable, const std::vector<std::wstring>& arguments)
        {
            auto commandLine = _quoteArgument(executable.native());
            for (const auto& argument : arguments)
            {
                commandLine.push_back(L' ');
                commandLine.append(_quoteArgument(argument));
            }
            return commandLine;
        }

        std::vector<wchar_t> _buildEnvironment()
        {
            std::map<std::wstring, std::wstring, CaseInsensitiveLess> variables;
            const auto environment = wil::unique_environstrings_ptr{ GetEnvironmentStringsW() };
            if (!environment)
            {
                return {};
            }

            for (auto current = environment.get(); *current; current += wcslen(current) + 1)
            {
                const std::wstring_view entry{ current };
                const auto separator = entry.find(L'=', entry.starts_with(L'=') ? 1 : 0);
                if (separator != std::wstring_view::npos)
                {
                    variables.insert_or_assign(std::wstring{ entry.substr(0, separator) }, std::wstring{ entry.substr(separator + 1) });
                }
            }

            for (const auto name : {
                     L"GIT_DIR",
                     L"GIT_WORK_TREE",
                     L"GIT_INDEX_FILE",
                     L"GIT_COMMON_DIR",
                     L"GIT_OBJECT_DIRECTORY",
                     L"GIT_ALTERNATE_OBJECT_DIRECTORIES",
                 })
            {
                variables.erase(name);
            }
            variables.insert_or_assign(L"GIT_OPTIONAL_LOCKS", L"0");
            variables.insert_or_assign(L"GIT_TERMINAL_PROMPT", L"0");
            variables.insert_or_assign(L"GCM_INTERACTIVE", L"never");

            size_t length = 1;
            for (const auto& [name, value] : variables)
            {
                length += name.size() + value.size() + 2;
            }

            std::vector<wchar_t> block;
            block.reserve(length);
            for (const auto& [name, value] : variables)
            {
                block.insert(block.end(), name.begin(), name.end());
                block.push_back(L'=');
                block.insert(block.end(), value.begin(), value.end());
                block.push_back(L'\0');
            }
            block.push_back(L'\0');
            return block;
        }

        bool _isRegularFile(const std::filesystem::path& path) noexcept
        {
            const auto attributes = GetFileAttributesW(path.c_str());
            return attributes != INVALID_FILE_ATTRIBUTES && (attributes & FILE_ATTRIBUTE_DIRECTORY) == 0;
        }

        bool _drainPipe(const HANDLE pipe, std::string& destination, size_t& totalBytes, const size_t maxOutputBytes)
        {
            std::array<char, 16 * 1024> buffer;
            for (;;)
            {
                DWORD available = 0;
                if (!PeekNamedPipe(pipe, nullptr, 0, nullptr, &available, nullptr) || available == 0)
                {
                    return true;
                }

                const auto requested = static_cast<DWORD>(std::min<size_t>(available, buffer.size()));
                DWORD read = 0;
                if (!ReadFile(pipe, buffer.data(), requested, &read, nullptr))
                {
                    return GetLastError() == ERROR_BROKEN_PIPE;
                }
                if (read > maxOutputBytes - std::min(totalBytes, maxOutputBytes))
                {
                    return false;
                }

                destination.append(buffer.data(), read);
                totalBytes += read;
            }
        }
    }

    GitProcessRunner::GitProcessRunner(std::filesystem::path gitExecutable) :
        _gitExecutable{ std::move(gitExecutable) }
    {
        if (!_gitExecutable.is_absolute() || !_isRegularFile(_gitExecutable))
        {
            _gitExecutable.clear();
        }
    }

    std::optional<std::filesystem::path> GitProcessRunner::FindGitExecutable()
    {
        const auto required = GetEnvironmentVariableW(L"PATH", nullptr, 0);
        if (required == 0)
        {
            return std::nullopt;
        }

        std::wstring pathValue(required, L'\0');
        const auto written = GetEnvironmentVariableW(L"PATH", pathValue.data(), required);
        if (written == 0 || written >= required)
        {
            return std::nullopt;
        }
        pathValue.resize(written);

        size_t offset = 0;
        while (offset <= pathValue.size())
        {
            const auto separator = pathValue.find(L';', offset);
            auto entry = pathValue.substr(offset, separator == std::wstring::npos ? std::wstring::npos : separator - offset);
            if (entry.size() >= 2 && entry.front() == L'"' && entry.back() == L'"')
            {
                entry = entry.substr(1, entry.size() - 2);
            }

            std::filesystem::path directory{ entry };
            if (!entry.empty() && directory.is_absolute())
            {
                const auto candidate = (directory / L"git.exe").lexically_normal();
                if (_isRegularFile(candidate))
                {
                    return candidate;
                }
            }

            if (separator == std::wstring::npos)
            {
                break;
            }
            offset = separator + 1;
        }

        return std::nullopt;
    }

    bool GitProcessRunner::IsValid() const noexcept
    {
        return !_gitExecutable.empty();
    }

    GitProcessResult GitProcessRunner::Run(const std::filesystem::path& workingDirectory,
                                           const std::vector<std::wstring>& arguments,
                                           const GitProcessOptions& options) const
    {
        GitProcessResult result;
        if (!IsValid() || !workingDirectory.is_absolute() || options.maxOutputBytes == 0)
        {
            result.win32Error = ERROR_INVALID_PARAMETER;
            return result;
        }

        SECURITY_ATTRIBUTES securityAttributes{ sizeof(securityAttributes), nullptr, TRUE };
        wil::unique_handle stdoutRead;
        wil::unique_handle stdoutWrite;
        wil::unique_handle stderrRead;
        wil::unique_handle stderrWrite;
        wil::unique_handle stdinHandle{ CreateFileW(L"NUL", GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE, &securityAttributes, OPEN_EXISTING, 0, nullptr) };
        if (!CreatePipe(stdoutRead.put(), stdoutWrite.put(), &securityAttributes, 0) ||
            !CreatePipe(stderrRead.put(), stderrWrite.put(), &securityAttributes, 0) ||
            !stdinHandle ||
            !SetHandleInformation(stdoutRead.get(), HANDLE_FLAG_INHERIT, 0) ||
            !SetHandleInformation(stderrRead.get(), HANDLE_FLAG_INHERIT, 0))
        {
            result.win32Error = GetLastError();
            return result;
        }

        SIZE_T attributeListSize = 0;
        InitializeProcThreadAttributeList(nullptr, 1, 0, &attributeListSize);
        auto attributeList = std::make_unique<std::byte[]>(attributeListSize);
        auto startupInfo = STARTUPINFOEXW{};
        startupInfo.StartupInfo.cb = sizeof(startupInfo);
        startupInfo.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startupInfo.StartupInfo.hStdInput = stdinHandle.get();
        startupInfo.StartupInfo.hStdOutput = stdoutWrite.get();
        startupInfo.StartupInfo.hStdError = stderrWrite.get();
        startupInfo.lpAttributeList = reinterpret_cast<PPROC_THREAD_ATTRIBUTE_LIST>(attributeList.get());
        if (!InitializeProcThreadAttributeList(startupInfo.lpAttributeList, 1, 0, &attributeListSize))
        {
            result.win32Error = GetLastError();
            return result;
        }
        const auto deleteAttributeList = wil::scope_exit([&] {
            DeleteProcThreadAttributeList(startupInfo.lpAttributeList);
        });

        std::array<HANDLE, 3> inheritedHandles{ stdinHandle.get(), stdoutWrite.get(), stderrWrite.get() };
        if (!UpdateProcThreadAttribute(
                startupInfo.lpAttributeList,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
                inheritedHandles.data(),
                sizeof(inheritedHandles),
                nullptr,
                nullptr))
        {
            result.win32Error = GetLastError();
            return result;
        }

        wil::unique_handle job{ CreateJobObjectW(nullptr, nullptr) };
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION jobLimits{};
        jobLimits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if (!job || !SetInformationJobObject(job.get(), JobObjectExtendedLimitInformation, &jobLimits, sizeof(jobLimits)))
        {
            result.win32Error = GetLastError();
            return result;
        }

        auto commandLine = _buildCommandLine(_gitExecutable, arguments);
        auto environment = _buildEnvironment();
        if (environment.empty())
        {
            result.win32Error = GetLastError();
            return result;
        }

        wil::unique_process_information process;
        if (!CreateProcessW(
                _gitExecutable.c_str(),
                commandLine.data(),
                nullptr,
                nullptr,
                TRUE,
                CREATE_NO_WINDOW | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
                environment.data(),
                workingDirectory.c_str(),
                &startupInfo.StartupInfo,
                &process))
        {
            result.win32Error = GetLastError();
            return result;
        }

        if (!AssignProcessToJobObject(job.get(), process.hProcess))
        {
            result.win32Error = GetLastError();
            TerminateProcess(process.hProcess, result.win32Error);
            return result;
        }
        if (ResumeThread(process.hThread) == static_cast<DWORD>(-1))
        {
            result.win32Error = GetLastError();
            TerminateJobObject(job.get(), result.win32Error);
            return result;
        }

        stdoutWrite.reset();
        stderrWrite.reset();
        stdinHandle.reset();

        const auto deadline = std::chrono::steady_clock::now() + options.timeout;
        size_t totalBytes = 0;
        for (;;)
        {
            if (!_drainPipe(stdoutRead.get(), result.standardOutput, totalBytes, options.maxOutputBytes) ||
                !_drainPipe(stderrRead.get(), result.standardError, totalBytes, options.maxOutputBytes))
            {
                result.status = GitProcessStatus::OutputLimitExceeded;
                TerminateJobObject(job.get(), ERROR_FILE_TOO_LARGE);
                break;
            }
            if (options.cancelled && options.cancelled->load(std::memory_order_relaxed))
            {
                result.status = GitProcessStatus::Cancelled;
                TerminateJobObject(job.get(), ERROR_CANCELLED);
                break;
            }
            if (std::chrono::steady_clock::now() >= deadline)
            {
                result.status = GitProcessStatus::TimedOut;
                TerminateJobObject(job.get(), WAIT_TIMEOUT);
                break;
            }

            const auto wait = WaitForSingleObject(process.hProcess, 10);
            if (wait == WAIT_OBJECT_0)
            {
                if (!_drainPipe(stdoutRead.get(), result.standardOutput, totalBytes, options.maxOutputBytes) ||
                    !_drainPipe(stderrRead.get(), result.standardError, totalBytes, options.maxOutputBytes))
                {
                    result.status = GitProcessStatus::OutputLimitExceeded;
                }
                else if (GetExitCodeProcess(process.hProcess, &result.exitCode))
                {
                    result.status = result.exitCode == 0 ? GitProcessStatus::Succeeded : GitProcessStatus::Failed;
                }
                else
                {
                    result.status = GitProcessStatus::Failed;
                    result.win32Error = GetLastError();
                }
                break;
            }
            if (wait == WAIT_FAILED)
            {
                result.status = GitProcessStatus::Failed;
                result.win32Error = GetLastError();
                TerminateJobObject(job.get(), result.win32Error);
                break;
            }
        }

        if (result.status == GitProcessStatus::TimedOut ||
            result.status == GitProcessStatus::Cancelled ||
            result.status == GitProcessStatus::OutputLimitExceeded)
        {
            WaitForSingleObject(process.hProcess, 1000);
        }
        return result;
    }
}
