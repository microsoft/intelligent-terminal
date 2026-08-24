// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "CommandRunner.h"

#include <windows.h>

#include <algorithm>
#include <array>
#include <atomic>
#include <filesystem>
#include <limits>
#include <mutex>
#include <thread>
#include <vector>

#include <wil/resource.h>

namespace Microsoft::Terminal::RichTab::Provider
{
    namespace
    {
        constexpr std::array environmentNames{
            L"APPDATA",
            L"HOMEDRIVE",
            L"HOMEPATH",
            L"LOCALAPPDATA",
            L"PATH",
            L"PATHEXT",
            L"PROGRAMDATA",
            L"PROGRAMFILES",
            L"PROGRAMFILES(X86)",
            L"PSMODULEPATH",
            L"SYSTEMDRIVE",
            L"SYSTEMROOT",
            L"TEMP",
            L"TMP",
            L"USERPROFILE",
            L"WINDIR",
        };

        std::optional<std::wstring> _ReadEnvironment(const wchar_t* name)
        {
            const auto required = GetEnvironmentVariableW(name, nullptr, 0);
            if (required == 0)
            {
                return std::nullopt;
            }
            std::wstring value(required, L'\0');
            const auto written = GetEnvironmentVariableW(name, value.data(), required);
            if (written == 0 || written >= required)
            {
                return std::nullopt;
            }
            value.resize(written);
            return value;
        }

        bool _IsReparsePoint(const std::filesystem::path& path) noexcept
        {
            const auto attributes = GetFileAttributesW(path.c_str());
            return attributes == INVALID_FILE_ATTRIBUTES ||
                   WI_IsFlagSet(attributes, FILE_ATTRIBUTE_REPARSE_POINT);
        }

        bool _PathHasReparsePoint(
            const std::filesystem::path& root,
            const std::filesystem::path& relative,
            uint32_t& error) noexcept
        {
            auto current = root;
            if (_IsReparsePoint(current))
            {
                error = GetLastError() == ERROR_SUCCESS ? ERROR_REPARSE_TAG_INVALID : GetLastError();
                return true;
            }
            for (const auto& part : relative)
            {
                current /= part;
                if (_IsReparsePoint(current))
                {
                    error = GetLastError() == ERROR_SUCCESS ? ERROR_REPARSE_TAG_INVALID : GetLastError();
                    return true;
                }
            }
            return false;
        }

        struct Pipe
        {
            wil::unique_handle read;
            wil::unique_handle write;
        };

        bool _CreatePipe(Pipe& pipe, const bool childReads)
        {
            SECURITY_ATTRIBUTES attributes{ sizeof(attributes), nullptr, TRUE };
            HANDLE read = nullptr;
            HANDLE write = nullptr;
            if (!CreatePipe(&read, &write, &attributes, 0))
            {
                return false;
            }
            pipe.read.reset(read);
            pipe.write.reset(write);
            return SetHandleInformation(
                       childReads ? pipe.write.get() : pipe.read.get(),
                       HANDLE_FLAG_INHERIT,
                       0) != FALSE;
        }

        void _ReadPipe(
            HANDLE pipe,
            const size_t limit,
            std::string& destination,
            std::atomic<bool>& exceeded)
        {
            std::array<char, 4096> buffer{};
            DWORD read = 0;
            while (ReadFile(pipe, buffer.data(), static_cast<DWORD>(buffer.size()), &read, nullptr) && read != 0)
            {
                const auto remaining = destination.size() < limit ? limit - destination.size() : 0;
                const auto copied = std::min<size_t>(remaining, read);
                destination.append(buffer.data(), copied);
                if (copied != read)
                {
                    exceeded.store(true, std::memory_order_release);
                }
            }
        }
    }

    std::optional<std::filesystem::path> CommandRunner::ResolvePowerShell()
    {
        const auto programFiles = _ReadEnvironment(L"PROGRAMFILES");
        if (!programFiles)
        {
            return std::nullopt;
        }
        const auto path = std::filesystem::path{ *programFiles } / L"PowerShell" / L"7" / L"pwsh.exe";
        uint32_t pathError = ERROR_SUCCESS;
        if (_PathHasReparsePoint(path.root_path(), path.relative_path(), pathError))
        {
            return std::nullopt;
        }
        const auto attributes = GetFileAttributesW(path.c_str());
        if (attributes == INVALID_FILE_ATTRIBUTES ||
            (attributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)) != 0)
        {
            return std::nullopt;
        }
        return path;
    }

    bool CommandRunner::ResolveEntrypoint(
        const Manifest& manifest,
        std::filesystem::path& resolved,
        uint32_t& error) noexcept
    {
        error = ERROR_SUCCESS;
        if (!IsSafeRelativeProviderPath(manifest.runtime.entrypoint) ||
            manifest.extensionRoot.empty() ||
            !manifest.extensionRoot.is_absolute())
        {
            error = ERROR_INVALID_NAME;
            return false;
        }

        try
        {
            if (_PathHasReparsePoint(
                    manifest.extensionRoot.root_path(),
                    manifest.extensionRoot.relative_path(),
                    error))
            {
                return false;
            }
            const auto root = std::filesystem::weakly_canonical(manifest.extensionRoot);
            const auto candidate = std::filesystem::weakly_canonical(root / manifest.runtime.entrypoint);
            const auto relative = candidate.lexically_relative(root);
            if (!IsSafeRelativeProviderPath(relative) ||
                _PathHasReparsePoint(root, relative, error))
            {
                if (error == ERROR_SUCCESS)
                {
                    error = ERROR_ACCESS_DENIED;
                }
                return false;
            }

            const auto attributes = GetFileAttributesW(candidate.c_str());
            if (attributes == INVALID_FILE_ATTRIBUTES ||
                (attributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)) != 0)
            {
                error = GetLastError();
                return false;
            }
            resolved = candidate;
            return true;
        }
        catch (...)
        {
            error = ERROR_INVALID_NAME;
            return false;
        }
    }

    void CommandRunner::QuoteArgument(const std::wstring_view argument, std::wstring& commandLine)
    {
        commandLine.push_back(L'"');
        size_t backslashes = 0;
        for (const auto ch : argument)
        {
            if (ch == L'\\')
            {
                ++backslashes;
            }
            else
            {
                if (ch == L'"')
                {
                    commandLine.append(backslashes * 2 + 1, L'\\');
                }
                else
                {
                    commandLine.append(backslashes, L'\\');
                }
                backslashes = 0;
                commandLine.push_back(ch);
            }
        }
        commandLine.append(backslashes * 2, L'\\');
        commandLine.push_back(L'"');
    }

    bool CommandRunner::BuildLaunchInfo(
        const Manifest& manifest,
        LaunchInfo& launch,
        uint32_t& error)
    {
        std::filesystem::path entrypoint;
        if (!ResolveEntrypoint(manifest, entrypoint, error))
        {
            return false;
        }

        std::vector<std::wstring> arguments;
        if (manifest.runtime.kind == RuntimeKind::PowerShellV1)
        {
            const auto powerShell = ResolvePowerShell();
            if (!powerShell)
            {
                error = ERROR_FILE_NOT_FOUND;
                return false;
            }
            launch.executable = *powerShell;
            arguments = {
                L"-NoLogo",
                L"-NoProfile",
                L"-NonInteractive",
                L"-File",
                entrypoint.native(),
            };
        }
        else
        {
            launch.executable = entrypoint;
        }
        arguments.insert(arguments.end(), manifest.runtime.arguments.begin(), manifest.runtime.arguments.end());

        launch.commandLine.clear();
        QuoteArgument(launch.executable.native(), launch.commandLine);
        for (const auto& argument : arguments)
        {
            launch.commandLine.push_back(L' ');
            QuoteArgument(argument, launch.commandLine);
        }
        error = ERROR_SUCCESS;
        return true;
    }

    std::wstring CommandRunner::BuildEnvironment(const Environment& extraEnvironment)
    {
        std::vector<std::pair<std::wstring, std::wstring>> variables;
        for (const auto name : environmentNames)
        {
            if (const auto value = _ReadEnvironment(name))
            {
                variables.emplace_back(name, *value);
            }
        }
        for (const auto& [name, value] : extraEnvironment)
        {
            if (!name.empty() && name.find(L'=') == std::wstring::npos &&
                value.find(L'\0') == std::wstring::npos)
            {
                variables.emplace_back(name, value);
            }
        }
        std::sort(variables.begin(), variables.end(), [](const auto& first, const auto& second) {
            return _wcsicmp(first.first.c_str(), second.first.c_str()) < 0;
        });

        std::wstring block;
        for (const auto& [name, value] : variables)
        {
            block.append(name);
            block.push_back(L'=');
            block.append(value);
            block.push_back(L'\0');
        }
        block.push_back(L'\0');
        return block;
    }

    CommandResult CommandRunner::Run(
        const Manifest& manifest,
        const std::string_view request,
        const std::chrono::milliseconds timeout,
        const Environment& extraEnvironment) const
    {
        CommandResult result;
        if (request.empty() ||
            request.size() > MaximumRequestSize ||
            timeout.count() <= 0 ||
            timeout > std::chrono::minutes{ 5 })
        {
            result.status = CommandResult::Status::InvalidRequest;
            result.win32Error = ERROR_INVALID_PARAMETER;
            return result;
        }

        LaunchInfo launch;
        if (!BuildLaunchInfo(manifest, launch, result.win32Error))
        {
            result.status = CommandResult::Status::ResolveFailed;
            return result;
        }

        Pipe input;
        Pipe output;
        Pipe error;
        if (!_CreatePipe(input, true) || !_CreatePipe(output, false) || !_CreatePipe(error, false))
        {
            result.win32Error = GetLastError();
            return result;
        }

        wil::unique_handle job{ CreateJobObjectW(nullptr, nullptr) };
        if (!job)
        {
            result.win32Error = GetLastError();
            return result;
        }
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits{};
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if (!SetInformationJobObject(job.get(), JobObjectExtendedLimitInformation, &limits, sizeof(limits)))
        {
            result.win32Error = GetLastError();
            return result;
        }

        STARTUPINFOEXW startup{};
        startup.StartupInfo.cb = sizeof(startup);
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = input.read.get();
        startup.StartupInfo.hStdOutput = output.write.get();
        startup.StartupInfo.hStdError = error.write.get();

        SIZE_T attributeSize = 0;
        InitializeProcThreadAttributeList(nullptr, 1, 0, &attributeSize);
        std::vector<std::byte> attributeStorage(attributeSize);
        startup.lpAttributeList = reinterpret_cast<PPROC_THREAD_ATTRIBUTE_LIST>(attributeStorage.data());
        if (!InitializeProcThreadAttributeList(startup.lpAttributeList, 1, 0, &attributeSize))
        {
            result.win32Error = GetLastError();
            return result;
        }
        const auto deleteAttributes = wil::scope_exit([&]() {
            DeleteProcThreadAttributeList(startup.lpAttributeList);
        });
        const std::array inheritedHandles{
            input.read.get(),
            output.write.get(),
            error.write.get(),
        };
        if (!UpdateProcThreadAttribute(
                startup.lpAttributeList,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
                const_cast<HANDLE*>(inheritedHandles.data()),
                sizeof(inheritedHandles),
                nullptr,
                nullptr))
        {
            result.win32Error = GetLastError();
            return result;
        }

        auto environment = BuildEnvironment(extraEnvironment);
        PROCESS_INFORMATION processInfo{};
        if (!CreateProcessW(
                launch.executable.c_str(),
                launch.commandLine.data(),
                nullptr,
                nullptr,
                TRUE,
                CREATE_NO_WINDOW | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
                environment.data(),
                manifest.extensionRoot.c_str(),
                &startup.StartupInfo,
                &processInfo))
        {
            result.win32Error = GetLastError();
            return result;
        }
        wil::unique_process_handle process{ processInfo.hProcess };
        wil::unique_handle thread{ processInfo.hThread };

        if (!AssignProcessToJobObject(job.get(), process.get()))
        {
            result.win32Error = GetLastError();
            TerminateProcess(process.get(), result.win32Error);
            return result;
        }

        input.read.reset();
        output.write.reset();
        error.write.reset();

        std::atomic<bool> stdoutExceeded{ false };
        std::atomic<bool> stderrExceeded{ false };
        std::thread stdoutReader{ _ReadPipe, output.read.get(), MaximumStandardOutputSize, std::ref(result.standardOutput), std::ref(stdoutExceeded) };
        std::thread stderrReader{ _ReadPipe, error.read.get(), MaximumStandardErrorSize, std::ref(result.standardError), std::ref(stderrExceeded) };
        const auto joinReaders = wil::scope_exit([&]() {
            output.read.reset();
            error.read.reset();
            if (stdoutReader.joinable())
                stdoutReader.join();
            if (stderrReader.joinable())
                stderrReader.join();
        });

        if (ResumeThread(thread.get()) == static_cast<DWORD>(-1))
        {
            result.win32Error = GetLastError();
            TerminateJobObject(job.get(), result.win32Error);
            input.write.reset();
            return result;
        }
        thread.reset();

        std::atomic<DWORD> writeError{ ERROR_SUCCESS };
        std::thread stdinWriter{ [&]() {
            size_t writtenTotal = 0;
            while (writtenTotal < request.size())
            {
                DWORD written = 0;
                const auto chunk = static_cast<DWORD>(std::min<size_t>(
                    request.size() - writtenTotal,
                    (std::numeric_limits<DWORD>::max)()));
                if (!WriteFile(
                        input.write.get(),
                        request.data() + writtenTotal,
                        chunk,
                        &written,
                        nullptr))
                {
                    writeError.store(GetLastError(), std::memory_order_release);
                    break;
                }
                writtenTotal += written;
            }
            input.write.reset();
        } };
        const auto joinWriter = wil::scope_exit([&]() {
            if (stdinWriter.joinable())
            {
                CancelSynchronousIo(stdinWriter.native_handle());
                stdinWriter.join();
            }
        });

        const auto wait = WaitForSingleObject(process.get(), static_cast<DWORD>(timeout.count()));
        if (wait == WAIT_TIMEOUT)
        {
            result.status = CommandResult::Status::TimedOut;
            TerminateJobObject(job.get(), ERROR_TIMEOUT);
            WaitForSingleObject(process.get(), 5000);
            stdinWriter.join();
            return result;
        }
        if (wait != WAIT_OBJECT_0)
        {
            result.status = CommandResult::Status::WaitFailed;
            result.win32Error = GetLastError();
            TerminateJobObject(job.get(), result.win32Error);
            stdinWriter.join();
            return result;
        }

        // The provider contract ends when the root process exits. Terminate
        // descendants before joining pipe workers so a child that inherited a
        // pipe cannot keep a blocked write/read alive past the request deadline.
        TerminateJobObject(job.get(), ERROR_PROCESS_ABORTED);
        stdinWriter.join();
        if (const auto inputError = writeError.load(std::memory_order_acquire);
            inputError != ERROR_SUCCESS && inputError != ERROR_BROKEN_PIPE && inputError != ERROR_NO_DATA)
        {
            result.status = CommandResult::Status::InputWriteFailed;
            result.win32Error = inputError;
            TerminateJobObject(job.get(), inputError);
            return result;
        }

        DWORD exitCode = 0;
        if (!GetExitCodeProcess(process.get(), &exitCode))
        {
            result.status = CommandResult::Status::ExitCodeUnavailable;
            result.win32Error = GetLastError();
            return result;
        }
        result.exitCode = exitCode;
        stdoutReader.join();
        stderrReader.join();
        if (stdoutExceeded.load(std::memory_order_acquire) ||
            stderrExceeded.load(std::memory_order_acquire))
        {
            result.status = CommandResult::Status::OutputLimitExceeded;
        }
        else
        {
            result.status = CommandResult::Status::Completed;
        }
        return result;
    }
}
